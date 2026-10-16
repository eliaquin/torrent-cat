//! Keep playback awake without leaving an assertion behind when the app exits.
use std::io;
use tokio::process::{Child, Command};

#[derive(Default)]
pub struct PlaybackPower {
    child: Option<Child>,
    shutting_down: bool,
}

impl PlaybackPower {
    pub fn ensure_open(&self) -> io::Result<()> {
        if self.shutting_down {
            return Err(io::Error::other("Application is shutting down"));
        }
        Ok(())
    }

    pub fn acquire(&mut self) -> io::Result<()> {
        self.ensure_open()?;
        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                return Ok(());
            }
        }
        self.child = Some(inhibit_until_exit(std::process::id())?);
        Ok(())
    }

    pub async fn release(&mut self) -> io::Result<()> {
        if let Some(mut child) = self.child.take() {
            child.kill().await?;
        }
        Ok(())
    }

    pub async fn shutdown(&mut self) -> io::Result<()> {
        self.shutting_down = true;
        self.release().await
    }
}

fn inhibit_until_exit(owner: u32) -> io::Result<Child> {
    Command::new("/usr/bin/caffeinate")
        // The OS watches our PID even when process::exit or a crash skips Drop.
        .args(["-d", "-i", "-w", &owner.to_string()])
        .kill_on_drop(true)
        .spawn()
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::{sleep, timeout};

    async fn assertions_for(pid: u32) -> String {
        let output = Command::new("/usr/bin/pmset")
            .args(["-g", "assertions"])
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .filter(|line| line.contains(&format!("pid {pid}(")))
            .collect::<Vec<_>>()
            .join("\n")
    }

    async fn wait_for_assertions(pid: u32) {
        timeout(Duration::from_secs(10), async {
            loop {
                let assertions = assertions_for(pid).await;
                if assertions.contains("PreventUserIdleDisplaySleep")
                    && assertions.contains("PreventUserIdleSystemSleep")
                {
                    break;
                }
                sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("playback should inhibit display and idle system sleep");
    }

    #[tokio::test]
    async fn player_close_releases_assertions_and_allows_reopening() {
        let mut power = PlaybackPower::default();
        for _ in 0..2 {
            power.acquire().unwrap();
            let pid = power.child.as_ref().unwrap().id().unwrap();
            wait_for_assertions(pid).await;
            power.acquire().unwrap();
            assert_eq!(power.child.as_ref().unwrap().id(), Some(pid));
            power.release().await.unwrap();
            assert!(assertions_for(pid).await.is_empty());
            power.release().await.unwrap();
        }
    }

    #[tokio::test]
    async fn app_shutdown_releases_assertions_and_rejects_late_playback() {
        let mut power = PlaybackPower::default();
        power.acquire().unwrap();
        let pid = power.child.as_ref().unwrap().id().unwrap();
        wait_for_assertions(pid).await;
        power.shutdown().await.unwrap();
        assert!(assertions_for(pid).await.is_empty());
        assert!(power.acquire().is_err());
        assert!(power.child.is_none());
        power.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn abrupt_owner_exit_releases_assertions_without_cleanup() {
        let mut owner = Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut child = inhibit_until_exit(owner.id().unwrap()).unwrap();
        let pid = child.id().unwrap();
        wait_for_assertions(pid).await;
        owner.kill().await.unwrap();
        timeout(Duration::from_secs(10), child.wait())
            .await
            .expect("caffeinate must exit even without the app's cleanup")
            .unwrap();
        assert!(assertions_for(pid).await.is_empty());
    }
}

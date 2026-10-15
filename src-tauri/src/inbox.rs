use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

#[derive(Default)]
pub struct Inbox {
    ready: bool,
    pending: Vec<String>,
    recent: HashMap<String, Instant>,
}
impl Inbox {
    pub fn push(&mut self, value: String) -> Option<String> {
        let value = if let Ok(url) = url::Url::parse(&value) {
            if url.scheme() == "file" {
                url.to_file_path().ok()?.to_string_lossy().into_owned()
            } else {
                value
            }
        } else {
            value
        };
        if !(value.starts_with("magnet:")
            || value.starts_with("stream-magnet:")
            || value.to_lowercase().ends_with(".torrent"))
        {
            return None;
        }
        self.recent
            .retain(|_, time| time.elapsed() < Duration::from_secs(2));
        if self.recent.contains_key(&value) {
            return None;
        }
        self.recent.insert(value.clone(), Instant::now());
        if self.ready {
            Some(value)
        } else {
            self.pending.push(value);
            None
        }
    }
    pub fn ready(&mut self) -> Vec<String> {
        self.ready = true;
        std::mem::take(&mut self.pending)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_inputs_are_queued_normalized_and_deduplicated() {
        let mut inbox = Inbox::default();
        assert_eq!(inbox.push("file:///tmp/One%20Two.torrent".into()), None);
        assert_eq!(inbox.push("/tmp/One Two.torrent".into()), None);
        assert_eq!(inbox.push("--unused-flag".into()), None);
        assert_eq!(inbox.ready(), vec!["/tmp/One Two.torrent"]);
        assert!(inbox.ready().is_empty());
        assert_eq!(
            inbox.push("/tmp/next.torrent".into()),
            Some("/tmp/next.torrent".into())
        );
        assert_eq!(inbox.push("/tmp/next.torrent".into()), None);
    }
}

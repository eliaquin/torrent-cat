use std::{
    io::IoSlice,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::Context;
use cap_std::fs::{Dir, OpenOptions};

use crate::{
    storage::{StorageFactoryExt, filesystem::opened_file::OurFileExt},
    torrent_state::{ManagedTorrentShared, TorrentMetadata},
};

use crate::storage::{StorageFactory, TorrentStorage};

use super::opened_file::OpenedFile;

#[derive(Default, Clone, Copy)]
pub struct FilesystemStorageFactory {}

impl StorageFactory for FilesystemStorageFactory {
    type Storage = FilesystemStorage;

    fn create(
        &self,
        shared: &ManagedTorrentShared,
        _metadata: &TorrentMetadata,
    ) -> anyhow::Result<FilesystemStorage> {
        Ok(FilesystemStorage {
            output_folder: shared.options.output_folder.clone(),
            root: Arc::new(open_root(&shared.options.output_folder)?),
            opened_files: Default::default(),
        })
    }

    fn clone_box(&self) -> crate::storage::BoxStorageFactory {
        self.boxed()
    }
}

pub struct FilesystemStorage {
    pub(crate) output_folder: PathBuf,
    // Keep the authority granted at creation. Never resolve torrent paths through
    // the ambient filesystem again, including after the folder is renamed.
    root: Arc<Dir>,
    pub(crate) opened_files: Vec<OpenedFile>,
}

fn open_root(path: &Path) -> anyhow::Result<Dir> {
    // The caller chooses this ambient root. All untrusted torrent paths below it
    // are resolved by cap-std, which confines even symlink traversal to this fd.
    // Strip trailing separators / `.` so O_NOFOLLOW applies to the actual
    // final directory rather than a synthetic trailing component.
    let path: PathBuf = path.components().collect();
    std::fs::create_dir_all(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nix::libc::O_DIRECTORY | nix::libc::O_NOFOLLOW | nix::libc::O_CLOEXEC)
            .open(&path)
            .context("download root must be a directory, not a symbolic link")?;
        Ok(Dir::from_std_file(file))
    }
    #[cfg(not(unix))]
    {
        Ok(Dir::open_ambient_dir(&path, cap_std::ambient_authority())?)
    }
}

impl FilesystemStorage {
    #[allow(dead_code)]
    pub(crate) fn take_fs(&self) -> anyhow::Result<Self> {
        Ok(Self {
            opened_files: self
                .opened_files
                .iter()
                .map(|f| f.take_clone())
                .collect::<anyhow::Result<Vec<_>>>()?,
            output_folder: self.output_folder.clone(),
            root: self.root.clone(),
        })
    }
}

impl TorrentStorage for FilesystemStorage {
    fn sync_all(&self) -> anyhow::Result<()> {
        for file in &self.opened_files {
            if !file.is_dummy() {
                file.lock_read()?.sync_all()?;
            }
        }
        Ok(())
    }

    fn pread_exact(&self, file_id: usize, offset: u64, buf: &mut [u8]) -> anyhow::Result<()> {
        self.opened_files
            .get(file_id)
            .context("no such file")?
            .lock_read()?
            .pread_exact(offset, buf)
    }

    fn pwrite_all(&self, file_id: usize, offset: u64, buf: &[u8]) -> anyhow::Result<()> {
        let of = self.opened_files.get(file_id).context("no such file")?;
        #[cfg(windows)]
        return of.try_mark_sparse()?.pwrite_all(offset, buf);
        #[cfg(not(windows))]
        return of.lock_read()?.pwrite_all(offset, buf);
    }

    fn pwrite_all_vectored(
        &self,
        file_id: usize,
        offset: u64,
        bufs: [IoSlice<'_>; 2],
    ) -> anyhow::Result<usize> {
        let of = self.opened_files.get(file_id).context("no such file")?;
        #[cfg(windows)]
        return of.try_mark_sparse()?.pwrite_all_vectored(offset, bufs);
        #[cfg(not(windows))]
        return of.lock_read()?.pwrite_all_vectored(offset, bufs);
    }

    fn remove_file(&self, _file_id: usize, filename: &Path) -> anyhow::Result<()> {
        Ok(self.root.remove_file(filename)?)
    }

    fn ensure_file_length(&self, file_id: usize, len: u64) -> anyhow::Result<()> {
        let f = &self.opened_files.get(file_id).context("no such file")?;
        #[cfg(windows)]
        f.try_mark_sparse()?;
        Ok(f.lock_read()?.set_len(len)?)
    }

    fn take(&self) -> anyhow::Result<Box<dyn TorrentStorage>> {
        Ok(Box::new(Self {
            opened_files: self
                .opened_files
                .iter()
                .map(|f| f.take_clone())
                .collect::<anyhow::Result<Vec<_>>>()?,
            output_folder: self.output_folder.clone(),
            root: self.root.clone(),
        }))
    }

    fn remove_directory_if_empty(&self, path: &Path) -> anyhow::Result<()> {
        if path.as_os_str().is_empty() {
            // Do not re-resolve and delete the ambient root: it may now name a
            // different directory. Torrent-owned descendants are removed below.
            return Ok(());
        }
        match self.root.remove_dir(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => Ok(()),
            Err(e) => Err(e).with_context(|| format!("error removing {path:?}")),
        }
    }

    fn init(
        &mut self,
        shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        self.init_files(metadata, shared.options.allow_overwrite)
    }

    fn refresh(
        &mut self,
        _shared: &ManagedTorrentShared,
        metadata: &TorrentMetadata,
    ) -> anyhow::Result<()> {
        self.init_files(metadata, true)
    }
}

impl FilesystemStorage {
    fn init_files(
        &mut self,
        metadata: &TorrentMetadata,
        allow_overwrite: bool,
    ) -> anyhow::Result<()> {
        let mut files = Vec::<OpenedFile>::new();
        for file_details in metadata.file_infos.iter() {
            let mut full_path = self.output_folder.clone();
            let relative_path = &file_details.relative_filename;
            full_path.push(relative_path);

            if file_details.attrs.padding {
                files.push(OpenedFile::new_dummy());
                continue;
            };
            let parent = relative_path.parent().context("bug: no parent")?;
            if !parent.as_os_str().is_empty() {
                self.root.create_dir_all(parent)?;
            }
            let mut options = OpenOptions::new();
            options.read(true).write(true);
            if allow_overwrite {
                options.create(true);
            } else {
                options.create_new(true);
            }
            let f = self
                .root
                .open_with(relative_path, &options)
                .with_context(|| {
                    format!("error opening confined torrent file {relative_path:?}")
                })?;
            anyhow::ensure!(
                f.metadata()?.is_file(),
                "torrent path is not a regular file"
            );
            files.push(OpenedFile::new(full_path, f.into_std()));
        }

        self.opened_files = files;
        Ok(())
    }
}

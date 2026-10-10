//! Dragged files for sandboxed apps. A Flatpak or Snap can't open the host
//! paths in `text/uri-list`, so the drag also offers
//! `application/vnd.portal.filetransfer`: when a target asks for it, the files
//! are registered with the document portal's FileTransfer interface and the
//! target gets the transfer's key, which it trades for paths it can open.
//! Only regular files can be transferred; folders reach unsandboxed apps
//! through the URI list alone.

use std::{fs::File, io::Write as _, os::fd::OwnedFd, path::PathBuf};

use ashpd::documents::file_transfer::{FileTransfer, StartTransferOptions};

pub(crate) const MIME_TYPE: &str = "application/vnd.portal.filetransfer";

/// Whether any of the dragged entries can be transferred.
pub(crate) fn offers(entries: &[(PathBuf, bool)]) -> bool {
    entries.iter().any(|(_, is_dir)| !is_dir)
}

/// Registers the regular files with the portal and writes the key to `fd`,
/// on a thread of its own so the drag never waits on D-Bus. On failure the
/// target reads nothing and falls back to the URI list.
pub(crate) fn send(entries: Vec<(PathBuf, bool)>, fd: OwnedFd) {
    let spawned = std::thread::Builder::new()
        .name("drag file transfer".into())
        .spawn(move || {
            let key = smol::block_on(start(&entries));
            match key {
                Ok(key) => {
                    let mut target = File::from(fd);
                    if let Err(error) = target.write_all(key.as_bytes()) {
                        log::warn!("handing the file transfer key to the drop target: {error}");
                    }
                }
                Err(error) => log::warn!("starting a portal file transfer: {error}"),
            }
        });
    if let Err(error) = spawned {
        log::warn!("starting the file transfer thread: {error}");
    }
}

async fn start(entries: &[(PathBuf, bool)]) -> anyhow::Result<String> {
    let files = entries
        .iter()
        .filter(|(_, is_dir)| !is_dir)
        .map(|(path, _)| File::open(path))
        .collect::<std::io::Result<Vec<_>>>()?;
    let portal = FileTransfer::new().await?;
    // One retrieval per transfer: a target that asks again gets a new key.
    let key = portal
        .start_transfer(
            StartTransferOptions::default()
                .set_writeable(false)
                .set_auto_stop(true),
        )
        .await?;
    portal.add_files(&key, &files, Default::default()).await?;
    Ok(key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// Needs a document portal on the session bus; run it on a private bus.
    #[test]
    #[ignore]
    fn the_key_trades_for_the_dragged_files_not_the_folders() {
        let scratch = std::env::temp_dir().join(format!("gpui-transfer-{}", std::process::id()));
        std::fs::create_dir_all(scratch.join("folder")).unwrap();
        std::fs::write(scratch.join("photo.jpg"), b"jpeg").unwrap();
        let (mut reader, writer) = std::io::pipe().unwrap();
        send(
            vec![
                (scratch.join("photo.jpg"), false),
                (scratch.join("folder"), true),
            ],
            OwnedFd::from(writer),
        );
        let mut key = String::new();
        reader.read_to_string(&mut key).unwrap();
        assert!(!key.is_empty(), "a key");
        let files = smol::block_on(async {
            FileTransfer::new()
                .await
                .unwrap()
                .retrieve_files(&key, Default::default())
                .await
                .unwrap()
        });
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].ends_with("photo.jpg"), "{files:?}");
        std::fs::remove_dir_all(scratch).unwrap();
    }
}

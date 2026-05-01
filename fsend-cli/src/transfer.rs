use std::path::{Path, PathBuf};

use async_compression::tokio::write::GzipEncoder;
use async_trait::async_trait;
use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncWriteExt;

pub const BUF_SIZE: usize = 8192;
pub const PROTO_VERSION: &str = "0.1.0";
pub const FSEND_ALPN: &[u8] = b"fsend/0.1.0";

#[derive(Debug, PartialEq, Clone, Encode, Decode, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FileSendRecvTree {
    File {
        name: String,
        skip: u64,
        size: u64,
    },
    Dir {
        name: String,
        files: Vec<FileSendRecvTree>,
    },
}

impl FileSendRecvTree {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Dir { name, .. } => name,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Dir { files, .. } => files.iter().map(|f| f.size()).sum(),
        }
    }

    pub fn skip(&self) -> u64 {
        match self {
            Self::File { skip, .. } => *skip,
            Self::Dir { files, .. } => files.iter().map(|f| f.skip()).sum(),
        }
    }
}

#[derive(Debug, PartialEq, Clone, Encode, Decode, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FilesAvailable {
    File {
        name: String,
        size: u64,
    },
    Dir {
        name: String,
        files: Vec<FilesAvailable>,
    },
}

impl FilesAvailable {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Dir { name, .. } => name,
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            Self::File { size, .. } => *size,
            Self::Dir { files, .. } => files.iter().map(|f| f.size()).sum(),
        }
    }

    pub fn to_send_recv_tree(&self) -> FileSendRecvTree {
        match self {
            Self::File { name, size } => FileSendRecvTree::File {
                name: name.clone(),
                skip: 0,
                size: *size,
            },
            Self::Dir { name, files } => FileSendRecvTree::Dir {
                name: name.clone(),
                files: files.iter().map(|f| f.to_send_recv_tree()).collect(),
            },
        }
    }

    pub fn remove_skipped(&self, to_skip: &FilesToSkip) -> Option<FileSendRecvTree> {
        match (self, to_skip) {
            (Self::File { name, size }, FilesToSkip::File { name: sn, skip }) => {
                if name == sn && size <= skip {
                    None
                } else {
                    Some(FileSendRecvTree::File {
                        name: name.clone(),
                        skip: *skip,
                        size: *size,
                    })
                }
            }
            (
                Self::Dir { name, files },
                FilesToSkip::Dir {
                    name: sn,
                    files: sf,
                },
            ) => {
                assert_eq!(name, sn, "tree roots do not match");
                let mut remaining = Vec::new();
                for file in files {
                    if let Some(skip_file) = sf.iter().find(|s| s.name() == file.name()) {
                        if let Some(r) = file.remove_skipped(skip_file) {
                            remaining.push(r);
                        }
                    } else {
                        remaining.push(file.to_send_recv_tree());
                    }
                }
                if remaining.is_empty() {
                    None
                } else {
                    Some(FileSendRecvTree::Dir {
                        name: name.clone(),
                        files: remaining,
                    })
                }
            }
            _ => panic!("tree roots do not match"),
        }
    }

    pub fn get_skippable(&self, local: &FilesAvailable) -> Option<FilesToSkip> {
        match (self, local) {
            (Self::File { name, .. }, Self::File { name: ln, size: ls }) => {
                if name == ln {
                    Some(FilesToSkip::File {
                        name: name.clone(),
                        skip: *ls,
                    })
                } else {
                    None
                }
            }
            (
                Self::Dir { name, files },
                Self::Dir {
                    name: ln,
                    files: lf,
                },
            ) => {
                if name != ln {
                    return None;
                }
                let mut skippable = Vec::new();
                for file in files {
                    if let Some(lm) = lf.iter().find(|l| l.name() == file.name()) {
                        if let Some(s) = file.get_skippable(lm) {
                            skippable.push(s);
                        }
                    }
                }
                if skippable.is_empty() {
                    None
                } else {
                    Some(FilesToSkip::Dir {
                        name: name.clone(),
                        files: skippable,
                    })
                }
            }
            _ => None,
        }
    }
}

pub fn get_files_available(path: &Path) -> std::io::Result<FilesAvailable> {
    let name = path.file_name().unwrap().to_str().unwrap().to_string();
    if path.is_file() {
        Ok(FilesAvailable::File {
            name,
            size: path.metadata()?.len(),
        })
    } else {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(path)? {
            files.push(get_files_available(&entry?.path())?);
        }
        Ok(FilesAvailable::Dir { name, files })
    }
}

#[derive(Debug, PartialEq, Clone, Encode, Decode, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FilesToSkip {
    File {
        name: String,
        skip: u64,
    },
    Dir {
        name: String,
        files: Vec<FilesToSkip>,
    },
}

impl FilesToSkip {
    pub fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Dir { name, .. } => name,
        }
    }

    pub fn skip(&self) -> u64 {
        match self {
            Self::File { skip, .. } => *skip,
            Self::Dir { files, .. } => files.iter().map(|f| f.skip()).sum(),
        }
    }
}

#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SenderToReceiver {
    ConnRequest { version: String },
    FileInfo { files: Vec<FilesAvailable> },
}

#[derive(Debug, Clone, Encode, Decode, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReceiverToSender {
    WrongVersion { expected: String },
    Ok,
    RejectFiles,
    AcceptFilesSkip { files: Vec<Option<FilesToSkip>> },
}

pub async fn compress_gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = GzipEncoder::new(&mut out);
    encoder.write_all(data).await?;
    encoder.shutdown().await?;
    Ok(out)
}

pub async fn decompress_gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut decoder = async_compression::tokio::write::GzipDecoder::new(&mut out);
    decoder.write_all(data).await?;
    decoder.shutdown().await?;
    Ok(out)
}

#[derive(Debug, Error)]
pub enum TransferError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("connection: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("read: {0}")]
    Read(#[from] quinn::ReadError),
    #[error("packet: {0}")]
    Packet(#[from] PacketRecvError),
    #[error("wrong version: expected {0}, got {1}")]
    WrongVersion(String, String),
    #[error("files rejected")]
    FilesRejected,
    #[error("unexpected packet")]
    UnexpectedPacket,
    #[error("file does not exist: {0}")]
    FileNotFound(PathBuf),
    #[error("iroh: {0}")]
    Iroh(String),
    #[error("webrtc: {0}")]
    WebRtc(String),
}

#[derive(Debug, Error)]
pub enum PacketRecvError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode: {0}")]
    Decode(#[from] bincode::error::DecodeError),
    #[error("connection: {0}")]
    Connection(#[from] iroh::endpoint::ConnectionError),
    #[error("read: {0}")]
    Read(#[from] iroh::endpoint::ReadError),
}

pub struct SendArgs {
    pub files: Vec<PathBuf>,
}

pub struct ReceiveArgs {
    pub resume: bool,
}

pub type ProgressCb<'a> = &'a mut (dyn FnMut(&[(String, u64, u64)]) + Send);
pub type WaitingCb<'a> = &'a mut (dyn FnMut() + Send);
pub type DataCb<'a> = &'a mut (dyn FnMut(u64) + Send);
pub type AcceptFilesCb<'a> = &'a mut (dyn FnMut(&[FilesAvailable]) -> Option<PathBuf> + Send);

#[async_trait]
pub trait Transfer: Send {
    async fn connection_type_name(&self) -> String;

    async fn send_files(
        &mut self,
        args: SendArgs,
        initial_progress_cb: ProgressCb<'_>,
        waiting_cb: WaitingCb<'_>,
        write_cb: DataCb<'_>,
    ) -> Result<(), TransferError>;

    async fn receive_files(
        &mut self,
        args: ReceiveArgs,
        initial_progress_cb: ProgressCb<'_>,
        accept_files_cb: AcceptFilesCb<'_>,
        read_cb: DataCb<'_>,
    ) -> Result<(), TransferError>;

    async fn close(&mut self);
}

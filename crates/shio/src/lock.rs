//! Android 进程锁。
//!
//! redb 4.4 在 Android 上不提供可用的文件锁：它依赖的字节范围锁在 Android 上
//! 返回 `Unsupported`，最后会退化为“无锁”。这里用一个旁路 `.lock` 文件补上
//! `flock`：写句柄独占，只读句柄共享。非 Android 平台继续使用 redb 自带的锁，
//! 本模块不改变这些平台的行为。

#[cfg(target_os = "android")]
mod imp {
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::path::{Path, PathBuf};

    use rustix::fs::{flock, FlockOperation};
    use rustix::io::Errno;

    use crate::{Error, Result};

    /// 持有打开期间的数据文件锁；`Drop` 或 `close` 时释放。
    pub(crate) struct DataFileLock {
        _file: File,
    }

    impl DataFileLock {
        pub(crate) fn acquire(path: &Path, read_only: bool) -> Result<Self> {
            let lock_path = lock_path(path);
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .open(&lock_path)
                .map_err(Error::Io)?;
            let operation = if read_only {
                FlockOperation::NonBlockingLockShared
            } else {
                FlockOperation::NonBlockingLockExclusive
            };
            match flock(&file, operation) {
                Ok(()) => Ok(Self { _file: file }),
                Err(errno) if errno == Errno::WOULDBLOCK || errno == Errno::AGAIN => {
                    Err(Error::Locked)
                }
                Err(errno) => Err(Error::Io(io::Error::from_raw_os_error(
                    errno.raw_os_error(),
                ))),
            }
        }
    }

    fn lock_path(path: &Path) -> PathBuf {
        let mut value = path.as_os_str().to_os_string();
        value.push(".lock");
        PathBuf::from(value)
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    use std::path::Path;

    use crate::Result;

    /// 非 Android 平台由 redb 的文件锁负责。
    pub(crate) struct DataFileLock;

    impl DataFileLock {
        pub(crate) fn acquire(_path: &Path, _read_only: bool) -> Result<Self> {
            Ok(Self)
        }
    }
}

pub(crate) use imp::DataFileLock;

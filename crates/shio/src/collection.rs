//! 集合句柄。
//!
//! 构造不访问磁盘；集合在首次写入时登记到 `catalog`。所有方法把同步引擎调用
//! 搬到阻塞线程池：写在调用线程序列化，读在阻塞线程反序列化。

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;
use shio_core::model::{ListOptions, Page, RecordId, RecordMeta, Stored, WriteOp};
use shio_core::store::RawWriteOp;
use zeroize::Zeroizing;

use crate::kernel::Inner;
use crate::{Error, Result};

/// 某命名空间下的一个记录集合。
pub struct Collection<T> {
    pub(crate) inner: Arc<Inner>,
    namespace: String,
    name: String,
    marker: PhantomData<T>,
}

/// 批量写在进入阻塞线程前准备好的操作：载荷已序列化并置零封装。
enum PreparedOp {
    Put {
        id: RecordId,
        plaintext: Zeroizing<Vec<u8>>,
    },
    Delete {
        id: RecordId,
    },
}

impl<T> Collection<T>
where
    T: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    pub(crate) fn new(inner: Arc<Inner>, namespace: String, name: String) -> Self {
        Self {
            inner,
            namespace,
            name,
            marker: PhantomData,
        }
    }

    /// 所属命名空间名称。
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// 集合名称。
    pub fn name(&self) -> &str {
        &self.name
    }

    /// 生成 UUIDv7 并插入。
    pub async fn insert(&self, value: &T) -> Result<(RecordId, RecordMeta)> {
        let plaintext = Zeroizing::new(serde_json::to_vec(value).map_err(Error::Serialize)?);
        let namespace = self.namespace.clone();
        let name = self.name.clone();
        self.inner
            .run_write(move |engine| engine.insert_raw(&namespace, &name, plaintext.as_slice()))
            .await
    }

    /// 指定 ID 的 upsert；墓碑记录会被复活，`rev` 递增。
    pub async fn put(&self, id: RecordId, value: &T) -> Result<RecordMeta> {
        let plaintext = Zeroizing::new(serde_json::to_vec(value).map_err(Error::Serialize)?);
        let namespace = self.namespace.clone();
        let name = self.name.clone();
        self.inner
            .run_write(move |engine| engine.put_raw(&namespace, &name, id, plaintext.as_slice(), 1))
            .await
    }

    /// 读取一条记录；墓碑与不存在的记录都返回 `None`。
    pub async fn get(&self, id: &RecordId) -> Result<Option<Stored<T>>> {
        let id = *id;
        let namespace = self.namespace.clone();
        let name = self.name.clone();
        self.inner
            .run(move |engine| {
                let Some((meta, plaintext)) = engine.get_raw(&namespace, &name, &id)? else {
                    return Ok(None);
                };
                let value = serde_json::from_slice(&plaintext).map_err(Error::Deserialize)?;
                Ok(Some(Stored { meta, value }))
            })
            .await
    }

    /// 写墓碑，返回记录是否存在。
    pub async fn delete(&self, id: &RecordId) -> Result<bool> {
        let id = *id;
        let namespace = self.namespace.clone();
        let name = self.name.clone();
        self.inner
            .run_write(move |engine| engine.delete(&namespace, &name, &id))
            .await
    }

    /// 物理删除，本地语义；多端同步启用后不得使用。
    pub async fn purge(&self, id: &RecordId) -> Result<bool> {
        let id = *id;
        let namespace = self.namespace.clone();
        let name = self.name.clone();
        self.inner
            .run_write(move |engine| engine.purge(&namespace, &name, &id))
            .await
    }

    /// 按记录 ID 升序分页；只读 header，不解密。
    pub async fn list(&self, options: ListOptions) -> Result<Page<RecordMeta>> {
        let namespace = self.namespace.clone();
        let name = self.name.clone();
        self.inner
            .run(move |engine| engine.list(&namespace, &name, options))
            .await
    }

    /// 原子批量写；任一操作失败则全部不生效。
    pub async fn batch(&self, ops: Vec<WriteOp<T>>) -> Result<Vec<RecordMeta>> {
        let mut encoded = Vec::new();
        for op in &ops {
            if let WriteOp::Put { value, .. } = op {
                encoded.push(Zeroizing::new(
                    serde_json::to_vec(value).map_err(Error::Serialize)?,
                ));
            }
        }

        let mut encoded = encoded.into_iter();
        let mut prepared = Vec::with_capacity(ops.len());
        for op in ops {
            match op {
                WriteOp::Put { id, .. } => {
                    let plaintext = encoded
                        .next()
                        .ok_or(Error::Internal("batch encoding mismatch"))?;
                    prepared.push(PreparedOp::Put { id, plaintext });
                }
                WriteOp::Delete { id } => prepared.push(PreparedOp::Delete { id }),
            }
        }

        let namespace = self.namespace.clone();
        let name = self.name.clone();
        self.inner
            .run_write(move |engine| {
                let raw: Vec<RawWriteOp<'_>> = prepared
                    .iter()
                    .map(|op| match op {
                        PreparedOp::Put { id, plaintext } => RawWriteOp::Put {
                            id: *id,
                            plaintext: plaintext.as_slice(),
                        },
                        PreparedOp::Delete { id } => RawWriteOp::Delete { id: *id },
                    })
                    .collect();
                engine.batch_raw(&namespace, &name, &raw)
            })
            .await
    }
}

impl<T> Clone for Collection<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            namespace: self.namespace.clone(),
            name: self.name.clone(),
            marker: PhantomData,
        }
    }
}

impl<T> fmt::Debug for Collection<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Collection")
            .field("namespace", &self.namespace)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

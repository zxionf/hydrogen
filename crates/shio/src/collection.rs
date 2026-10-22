//! 集合句柄。
//!
//! 构造不访问磁盘；集合在首次写入时登记到 `catalog`。读写方法在存储层接线后
//! 按 `.design/api.md` §4.5 补齐。

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::kernel::Inner;

/// 某命名空间下的一个记录集合。
pub struct Collection<T> {
    #[allow(dead_code)]
    pub(crate) inner: Arc<Inner>,
    namespace: String,
    name: String,
    marker: PhantomData<T>,
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

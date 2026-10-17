//! 隐藏的会话（docs/design/agents.md「本 crate 的公开项」）。

use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use lectern_services::session::SessionKey;

/// 不出现在会话列表里、也不交出记录的会话。Analyze 的后续 issue 用它隐藏辅助会话（它们本来写在临时的
/// 配置目录里，这是兜底）。`Clone` 共享同一份集合；任何线程可调用，不阻塞。下一次发现（或监听触发的刷新）
/// 发布列表时去掉它；`read`、`turns`、`load` 从调用 `hide` 起返回 `NotFound`。
#[derive(Clone, Default, Debug)]
pub struct SessionFilter {
    hidden: Arc<RwLock<HashSet<SessionKey>>>,
}

impl SessionFilter {
    pub fn new() -> SessionFilter {
        SessionFilter::default()
    }

    pub fn hide(&self, session: SessionKey) {
        self.hidden
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session);
    }

    pub(crate) fn is_hidden(&self, session: &SessionKey) -> bool {
        self.hidden
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains(session)
    }
}

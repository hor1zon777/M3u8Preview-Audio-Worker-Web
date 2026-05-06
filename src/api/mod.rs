// api/mod.rs：服务器 API 客户端模块入口。

pub mod client;

pub use client::{ApiClient, ApiError, AudioCompleteMeta, AudioFetchTask, ClaimedJob, RegisterResponse};

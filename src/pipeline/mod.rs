// pipeline/mod.rs：audio worker 业务流水线模块入口。
//
// 与字幕项目（m3u8-preview-worker）的 pipeline 相比：
//   - 删除了 transcriber / segmenter / translator / vtt 四个 ASR/翻译相关模块
//   - 删除了 manual（独立测试模式）
//   - 新增 intermediate（FLAC 编码 + SHA256 + 时长探测）
//   - 新增 audio_owner（v3 broker 模式：本地 FLAC 仓库 + long-poll fetch loop）

pub mod audio_owner;
pub mod downloader;
pub mod extractor;
pub mod intermediate;
pub mod m3u8_parser;
pub mod poller;
pub mod proc_util;
pub mod runner;
pub mod tools;

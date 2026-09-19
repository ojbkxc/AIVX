//! aivx 主入口（P0：骨架占位——启动即自检通过后退出）。
//!
//! 生产编排（P1 接入）：ConfigHolder → CameraManager（拉线程束）→
//! forwarder → DbWriter → Projector → axum。P0 先让 CI 有一个可编译可跑的 bin。

fn main() {
    println!("AIVX {} — P0 skeleton", env!("CARGO_PKG_VERSION"));
    println!("perception: no-tokio plane OK");
}

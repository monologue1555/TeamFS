//! Timer owns no second copy of filesystem state; all work uses the service mutex.
use crate::service::Service;
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct AutoSync {
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}
impl AutoSync {
    pub fn start(service: Arc<Mutex<Service>>, seconds: u64) -> Result<Self, String> {
        let (stop, receiver) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("teamfs-auto-sync".into())
            .spawn(move || loop {
                match receiver.recv_timeout(Duration::from_secs(seconds)) {
                    Err(mpsc::RecvTimeoutError::Timeout) => match service.lock() {
                        Ok(mut service) => service.auto_sync(),
                        Err(_) => {
                            eprintln!("TeamFS 自动同步停止：服务锁损坏");
                            break;
                        }
                    },
                    _ => break,
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            stop,
            worker: Some(worker),
        })
    }
    pub fn stop(mut self) -> Result<(), String> {
        let _ = self.stop.send(());
        self.worker
            .take()
            .unwrap()
            .join()
            .map_err(|_| "自动同步线程异常退出".into())
    }
}
impl Drop for AutoSync {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

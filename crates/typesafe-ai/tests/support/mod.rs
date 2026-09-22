use serde_json::Value;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
pub struct Reply {
    pub status: u16,
    pub body: String,
    pub headers: String,
    pub delay: Duration,
}
impl Reply {
    pub fn json(status: u16, value: Value) -> Self {
        Self {
            status,
            body: value.to_string(),
            headers: String::new(),
            delay: Duration::ZERO,
        }
    }
}
pub struct Server {
    pub endpoint: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    task: Option<JoinHandle<()>>,
}
impl Server {
    pub async fn start(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let saved = requests.clone();
        let task = tokio::spawn(async move {
            for reply in replies {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let mut buffer = [0; 4096];
                let header_end = loop {
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&buffer[..n]);
                    if let Some(i) = data.windows(4).position(|b| b == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let headers = String::from_utf8(data[..header_end].to_vec()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                while data.len() < header_end + length {
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert!(n > 0);
                    data.extend_from_slice(&buffer[..n]);
                }
                let body = serde_json::from_slice(&data[header_end..header_end + length]).unwrap();
                saved.lock().unwrap().push((headers, body));
                tokio::time::sleep(reply.delay).await;
                let wire = format!("HTTP/1.1 {} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}", reply.status, reply.body.len(), reply.headers, reply.body);
                let _ = stream.write_all(wire.as_bytes()).await;
            }
        });
        Self {
            endpoint,
            requests,
            task: Some(task),
        }
    }
    pub fn requests(&self) -> Vec<(String, Value)> {
        self.requests.lock().unwrap().clone()
    }
    pub async fn finish(mut self) {
        self.task.take().unwrap().await.unwrap();
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

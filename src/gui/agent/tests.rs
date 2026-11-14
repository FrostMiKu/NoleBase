use super::*;
use crate::core::{storage::Storage, workspace::Workspace};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    time::{Duration, Instant},
};

#[test]
fn desktop_agent_streams_from_the_provider_and_restores_its_session() {
    let root = tempfile::tempdir().unwrap();
    let storage = Storage::new(root.path()).unwrap();
    storage.ensure_files().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    std::fs::write(&storage.ai_config_path, format!("api_format = 'completions'\napi_key = 'test'\nmodel = 'test'\nbase_url = 'http://{address}'\n")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "Agent did not reach the provider"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("accept: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
        }
        let mut request = vec![0; length];
        reader.read_exact(&mut request).unwrap();
        let response = "data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"desktop\"},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        write!(reader.get_mut(), "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        String::from_utf8(request).unwrap()
    });
    let workspace = Arc::new(Workspace::open(storage).unwrap());
    let mut client = AgentClient::open(AgentSessions::open(workspace.clone()).unwrap()).unwrap();
    client.send("Find my notes\nwith context".into()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        client.poll();
        if !client.current.session.is_running()
            && client.panel.last_assistant_output() == Some("Hello desktop")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "Agent did not complete: {:?}",
            client.panel.entries()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let request: serde_json::Value = serde_json::from_str(&server.join().unwrap()).unwrap();
    assert!(request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|message| message["role"] == "user"
            && message["content"]
                .as_str()
                .is_some_and(|text| text.contains("Find my notes"))));
    let id = client.current.summary().id;
    client.current.persist().unwrap();
    let restored = AgentClient::open(AgentSessions::open(workspace).unwrap()).unwrap();
    assert_eq!(restored.current.summary().id, id);
    assert_eq!(
        restored.panel.last_assistant_output(),
        Some("Hello desktop")
    );
    assert!(restored.panel.entries().iter().any(|entry| matches!(entry.as_ref(), AgentPanelEntry::Prompt { text, .. } if text == "Find my notes\nwith context")));
    assert!(restored
        .rows
        .iter()
        .any(|row| row.text.as_ref() == "Hello desktop"));
}

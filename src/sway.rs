use serde::Deserialize;
use std::env;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use eyre::{Result, eyre};

const MAGIC: &[u8] = b"i3-ipc";

#[repr(u32)]
#[derive(Copy, Clone)]
enum MessageType {
    Command = 0,
    GetTree = 4,
}

pub struct Client {
    stream: UnixStream,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct Node {
    id: i64,
    name: Option<String>,
    focused: bool,
    fullscreen_mode: Option<u8>,
    nodes: Vec<Node>,
    floating_nodes: Vec<Node>,
}

impl Client {
    pub fn connect() -> Result<Self> {
        let socket_path = env::var("SWAYSOCK")
            .map_err(|_| eyre!("SWAYSOCK not set"))?;
        let stream = UnixStream::connect(socket_path)?;
        Ok(Self { stream })
    }

    fn send_message(&mut self, msg_type: MessageType, payload: &str) -> Result<()> {
        let payload_bytes = payload.as_bytes();
        let len = payload_bytes.len() as u32;
        
        self.stream.write_all(MAGIC)?;
        self.stream.write_all(&len.to_ne_bytes())?;
        self.stream.write_all(&(msg_type as u32).to_ne_bytes())?;
        self.stream.write_all(payload_bytes)?;
        
        Ok(())
    }

    fn receive_message(&mut self) -> Result<String> {
        let mut magic = [0u8; 6];
        self.stream.read_exact(&mut magic)?;
        
        if magic != MAGIC {
            return Err(eyre!("Invalid magic string"));
        }

        let mut len_bytes = [0u8; 4];
        self.stream.read_exact(&mut len_bytes)?;
        let len = u32::from_ne_bytes(len_bytes);

        let mut type_bytes = [0u8; 4];
        self.stream.read_exact(&mut type_bytes)?;
        // We can ignore the type for now, assuming it matches request

        let mut payload = vec![0u8; len as usize];
        self.stream.read_exact(&mut payload)?;

        Ok(String::from_utf8(payload)?)
    }

    pub fn get_tree(&mut self) -> Result<Node> {
        self.send_message(MessageType::GetTree, "")?;
        let resp = self.receive_message()?;
        let node: Node = serde_json::from_str(&resp)?;
        Ok(node)
    }

    pub fn exec(&mut self, cmd: &str) -> Result<()> {
        let payload = format!("exec {}", cmd);
        self.send_message(MessageType::Command, &payload)?;
        let _resp = self.receive_message()?;
        // We could check "success" in response but typically exec succeeds in queueing
        Ok(())
    }

    pub fn run_command(&mut self, cmd: &str) -> Result<()> {
        self.send_message(MessageType::Command, cmd)?;
        let _resp = self.receive_message()?;
        Ok(())
    }

    pub fn set_fullscreen(&mut self, enable: bool, node_id: Option<i64>) -> Result<()> {
        let cmd = match (enable, node_id) {
            (true, Some(id)) => format!("[con_id={}] fullscreen enable", id),
            (false, Some(id)) => format!("[con_id={}] fullscreen disable", id),
            (true, None) => "fullscreen enable".to_string(),
            (false, None) => "fullscreen disable".to_string(),
        };
        self.send_message(MessageType::Command, &cmd)?;
        let _resp = self.receive_message()?;
        Ok(())
    }

    pub fn get_focused_fullscreen_node_id(&mut self) -> Result<Option<i64>> {
        let tree = self.get_tree()?;
        if let Some(node) = find_focused_node(&tree) {
             // fullscreen_mode: 0 (none), 1 (output), 2 (global)
             if node.fullscreen_mode.unwrap_or(0) > 0 {
                 Ok(Some(node.id))
             } else {
                 Ok(None)
             }
        } else {
            Ok(None)
        }
    }
}

fn find_focused_node(node: &Node) -> Option<&Node> {
    if node.focused {
        return Some(node);
    }

    for child in &node.nodes {
        if let Some(found) = find_focused_node(child) {
            return Some(found);
        }
    }
    
    for child in &node.floating_nodes {
        if let Some(found) = find_focused_node(child) {
            return Some(found);
        }
    }

    None
}

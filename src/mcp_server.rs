use std::io::{BufRead, Write};
use crossbeam_channel::{bounded, Sender};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug)]
pub enum McpCommand {
    OpenFolder {
        path: String,
        resp: Sender<Result<String, String>>,
    },
    SelectImage {
        filename: String,
        resp: Sender<Result<String, String>>,
    },
    RotateImage {
        direction: String,
        resp: Sender<Result<String, String>>,
    },
    SetZoom {
        mode: String,
        factor: Option<f32>,
        resp: Sender<Result<String, String>>,
    },
    SetRating {
        rating: i32,
        resp: Sender<Result<String, String>>,
    },
    FilterImages {
        query: Option<String>,
        min_rating: Option<i32>,
        format: Option<String>,
        resp: Sender<Result<String, String>>,
    },
    GetCurrentImageInfo {
        resp: Sender<Result<Value, String>>,
    },
    ListImages {
        resp: Sender<Result<Vec<String>, String>>,
    },
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

pub fn get_tools_definition() -> Value {
    json!({
        "tools": [
            {
                "name": "open_folder",
                "description": "Open a folder in Horama and load its images and subfolders into the view",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Absolute filesystem path to the folder"
                        }
                    },
                    "required": ["path"]
                }
            },
            {
                "name": "select_image",
                "description": "Select and display an image by filename or absolute path in the main viewport",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "filename": {
                            "type": "string",
                            "description": "Image file name (e.g. 'photo.jpg') or full path"
                        }
                    },
                    "required": ["filename"]
                }
            },
            {
                "name": "rotate_image",
                "description": "Rotate the currently displayed image. The rotation is saved to the file losslessly (JPEG/TIFF: EXIF orientation tag; PNG/WebP/BMP: lossless re-encode). RAW files are rotated on screen only.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "direction": {
                            "type": "string",
                            "enum": ["cw", "ccw", "180"],
                            "description": "Rotation direction: 'cw' (90 clockwise), 'ccw' (90 counter-clockwise), or '180'"
                        }
                    },
                    "required": ["direction"]
                }
            },
            {
                "name": "set_zoom",
                "description": "Set canvas zoom level: 'fit' (fit to window), '100' (100% 1:1 pixel view), or 'custom' with factor",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "mode": {
                            "type": "string",
                            "enum": ["fit", "100", "custom"],
                            "description": "Zoom mode"
                        },
                        "factor": {
                            "type": "number",
                            "description": "Zoom factor when mode is 'custom' (e.g. 1.5 for 150%)"
                        }
                    },
                    "required": ["mode"]
                }
            },
            {
                "name": "set_rating",
                "description": "Set star rating for the currently selected image (0 for unrated, 1 to 5 stars)",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "rating": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": 5,
                            "description": "Rating score (0 = clear/unrated, 1-5 = star rating)"
                        }
                    },
                    "required": ["rating"]
                }
            },
            {
                "name": "filter_images",
                "description": "Filter the visible image gallery by search text query, minimum star rating, or file format",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Partial search string for image filename (empty to clear)"
                        },
                        "min_rating": {
                            "type": "integer",
                            "description": "Minimum rating score: 0 for all, 1-5 for min stars, -1 for unrated only"
                        },
                        "format": {
                            "type": "string",
                            "enum": ["ALL", "RAW", "JPG", "PNG", "TIFF", "WEBP"],
                            "description": "Format filter ('ALL', 'RAW', 'JPG', 'PNG', 'TIFF', 'WEBP')"
                        }
                    }
                }
            },
            {
                "name": "get_current_image_info",
                "description": "Get detailed metadata, EXIF parameters, dimensions, rating, and color profile of the current image",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            },
            {
                "name": "list_images",
                "description": "List all images currently visible in the active gallery / folder view",
                "inputSchema": {
                    "type": "object",
                    "properties": {}
                }
            }
        ]
    })
}

fn tool_error_response(id: Value, msg: &str) -> String {
    let resp = JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result: Some(json!({ "content": [{ "type": "text", "text": msg }], "isError": true })),
        error: None,
    };
    serde_json::to_string(&resp).unwrap_or_default()
}

pub fn handle_jsonrpc_request(req_str: &str, tx: &Sender<McpCommand>) -> Option<String> {
    let req: JsonRpcRequest = match serde_json::from_str(req_str) {
        Ok(r) => r,
        Err(_) => {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id: Value::Null,
                result: None,
                error: Some(json!({"code": -32700, "message": "Parse error"})),
            };
            return Some(serde_json::to_string(&resp).unwrap());
        }
    };

    // JSON-RPC notifications (no "id") must never receive a response.
    let Some(id) = req.id.clone() else {
        return None;
    };
    let wait = |secs: u64| std::time::Duration::from_secs(secs);

    match req.method.as_str() {
        "initialize" => {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": "horama",
                        "version": "0.5.0"
                    }
                })),
                error: None,
            };
            Some(serde_json::to_string(&resp).unwrap())
        }
        "ping" => {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(json!({})),
                error: None,
            };
            Some(serde_json::to_string(&resp).unwrap())
        }
        "tools/list" => {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: Some(get_tools_definition()),
                error: None,
            };
            Some(serde_json::to_string(&resp).unwrap())
        }
        "tools/call" => {
            let tool_name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = req.params.get("arguments").cloned().unwrap_or(json!({}));

            let tool_res: Result<Value, String> = match tool_name {
                "open_folder" => {
                    let path = arguments.get("path").and_then(|v| v.as_str()).unwrap_or("");
                    if path.is_empty() {
                        Err("Missing 'path' argument".to_string())
                    } else {
                        let (resp_tx, resp_rx) = bounded(1);
                        let _ = tx.send(McpCommand::OpenFolder {
                            path: path.to_string(),
                            resp: resp_tx,
                        });
                        resp_rx.recv_timeout(wait(120))
                            .map_err(|e| format!("Timeout or error: {}", e))
                            .and_then(|res| res.map(|msg| json!({ "status": "success", "message": msg })))
                    }
                }
                "select_image" => {
                    let filename = arguments.get("filename").and_then(|v| v.as_str()).unwrap_or("");
                    if filename.is_empty() {
                        Err("Missing 'filename' argument".to_string())
                    } else {
                        let (resp_tx, resp_rx) = bounded(1);
                        let _ = tx.send(McpCommand::SelectImage {
                            filename: filename.to_string(),
                            resp: resp_tx,
                        });
                        resp_rx.recv_timeout(wait(30))
                            .map_err(|e| format!("Timeout or error: {}", e))
                            .and_then(|res| res.map(|msg| json!({ "status": "success", "message": msg })))
                    }
                }
                "rotate_image" => {
                    let dir = arguments.get("direction").and_then(|v| v.as_str()).unwrap_or("cw");
                    let (resp_tx, resp_rx) = bounded(1);
                    let _ = tx.send(McpCommand::RotateImage {
                        direction: dir.to_string(),
                        resp: resp_tx,
                    });
                    resp_rx.recv_timeout(wait(30))
                        .map_err(|e| format!("Timeout or error: {}", e))
                        .and_then(|res| res.map(|msg| json!({ "status": "success", "message": msg })))
                }
                "set_zoom" => {
                    let mode = arguments.get("mode").and_then(|v| v.as_str()).unwrap_or("fit");
                    let factor = arguments.get("factor").and_then(|v| v.as_f64()).map(|f| f as f32);
                    let (resp_tx, resp_rx) = bounded(1);
                    let _ = tx.send(McpCommand::SetZoom {
                        mode: mode.to_string(),
                        factor,
                        resp: resp_tx,
                    });
                    resp_rx.recv_timeout(wait(30))
                        .map_err(|e| format!("Timeout or error: {}", e))
                        .and_then(|res| res.map(|msg| json!({ "status": "success", "message": msg })))
                }
                "set_rating" => {
                    let rating = arguments.get("rating").and_then(|v| v.as_i64()).unwrap_or(-1);
                    if !(0..=5).contains(&rating) {
                        return Some(tool_error_response(id, "'rating' must be an integer between 0 and 5"));
                    }
                    let rating = rating as i32;
                    let (resp_tx, resp_rx) = bounded(1);
                    let _ = tx.send(McpCommand::SetRating {
                        rating,
                        resp: resp_tx,
                    });
                    resp_rx.recv_timeout(wait(30))
                        .map_err(|e| format!("Timeout or error: {}", e))
                        .and_then(|res| res.map(|msg| json!({ "status": "success", "message": msg })))
                }
                "filter_images" => {
                    let query = arguments.get("query").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let min_rating = arguments.get("min_rating").and_then(|v| v.as_i64()).map(|n| n as i32);
                    let format = arguments.get("format").and_then(|v| v.as_str()).map(|s| s.to_string());
                    let (resp_tx, resp_rx) = bounded(1);
                    let _ = tx.send(McpCommand::FilterImages {
                        query,
                        min_rating,
                        format,
                        resp: resp_tx,
                    });
                    resp_rx.recv_timeout(wait(30))
                        .map_err(|e| format!("Timeout or error: {}", e))
                        .and_then(|res| res.map(|msg| json!({ "status": "success", "message": msg })))
                }
                "get_current_image_info" => {
                    let (resp_tx, resp_rx) = bounded(1);
                    let _ = tx.send(McpCommand::GetCurrentImageInfo {
                        resp: resp_tx,
                    });
                    resp_rx.recv_timeout(wait(30))
                        .map_err(|e| format!("Timeout or error: {}", e))
                        .and_then(|res| res)
                }
                "list_images" => {
                    let (resp_tx, resp_rx) = bounded(1);
                    let _ = tx.send(McpCommand::ListImages {
                        resp: resp_tx,
                    });
                    resp_rx.recv_timeout(wait(30))
                        .map_err(|e| format!("Timeout or error: {}", e))
                        .and_then(|res| res.map(|files| json!({ "count": files.len(), "files": files })))
                }
                _ => Err(format!("Unknown tool: {}", tool_name)),
            };

            let resp = match tool_res {
                Ok(val) => JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(json!({
                        "content": [
                            {
                                "type": "text",
                                "text": serde_json::to_string_pretty(&val).unwrap_or_default()
                            }
                        ],
                        "isError": false
                    })),
                    error: None,
                },
                Err(err_msg) => JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: Some(json!({
                        "content": [
                            {
                                "type": "text",
                                "text": err_msg
                            }
                        ],
                        "isError": true
                    })),
                    error: None,
                },
            };
            Some(serde_json::to_string(&resp).unwrap())
        }
        _ => {
            let resp = JsonRpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(json!({
                    "code": -32601,
                    "message": format!("Method not found: {}", req.method)
                })),
            };
            Some(serde_json::to_string(&resp).unwrap())
        }
    }
}

pub fn run_mcp_stdio_server(tx: Sender<McpCommand>) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let reader = stdin.lock();

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(resp_str) = handle_jsonrpc_request(trimmed, &tx) {
            let _ = writeln!(stdout, "{}", resp_str);
            let _ = stdout.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mcp_initialize() {
        let (tx, _rx) = bounded(1);
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
        let resp_str = handle_jsonrpc_request(req, &tx).expect("Expected response");
        let val: Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(val["id"], 1);
        assert_eq!(val["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(val["result"]["serverInfo"]["name"], "horama");
    }

    #[test]
    fn notifications_get_no_response() {
        let (tx, _rx) = bounded(1);
        for m in ["notifications/initialized", "notifications/cancelled", "anything"] {
            let req = format!(r#"{{"jsonrpc":"2.0","method":"{}","params":{{}}}}"#, m);
            assert!(handle_jsonrpc_request(&req, &tx).is_none(), "{}", m);
        }
    }

    #[test]
    fn invalid_rating_is_rejected_without_touching_the_app() {
        let (tx, rx) = bounded(1);
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"set_rating","arguments":{"rating":-1}}}"#;
        let v: Value = serde_json::from_str(&handle_jsonrpc_request(req, &tx).unwrap()).unwrap();
        assert_eq!(v["result"]["isError"], true);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn test_mcp_tools_list() {
        let (tx, _rx) = bounded(1);
        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let resp_str = handle_jsonrpc_request(req, &tx).expect("Expected response");
        let val: Value = serde_json::from_str(&resp_str).unwrap();
        assert_eq!(val["id"], 2);
        let tools = val["result"]["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["name"] == "open_folder"));
        assert!(tools.iter().any(|t| t["name"] == "rotate_image"));
        assert!(tools.iter().any(|t| t["name"] == "set_zoom"));
        assert!(tools.iter().any(|t| t["name"] == "filter_images"));
    }
}

use dashmap::DashMap;
use regex::Regex;
use serde_json::Value;
use std::ops::Deref;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{debug, info};
use tree_sitter::{Node, Parser, Point};
use tree_sitter_nix::language;

#[derive(Clone, Debug)]
pub struct Backend {
    pub client: Client,
    pub document_map: DashMap<String, String>,
    pub completion_json: Value,
    pub last_cursor_position: DashMap<String, Position>,
    pub current_scope: DashMap<String, Vec<String>>,
}

impl Backend {
    fn get_scope(&self, root_node: Node, cursor_position: Point, source: &str) -> Vec<String> {
        let mut attrpaths = Vec::new();

        if let Some(node) = root_node.descendant_for_point_range(cursor_position, cursor_position) {
            let mut current_node = node;

            loop {
                if current_node.kind() == "attrpath" {
                    if let Ok(text) = current_node.utf8_text(source.as_bytes()) {
                        attrpaths.push(text.to_string());
                    }
                }

                if let Some(prev_sibling) = current_node.prev_sibling() {
                    current_node = prev_sibling;
                } else if let Some(parent) = current_node.parent() {
                    current_node = parent;
                } else {
                    break;
                }
            }
        }

        attrpaths.reverse();
        attrpaths
    }

    fn get_path(&self, line: &str) -> Vec<String> {
        let parts: Vec<&str> = line.split('.').collect();

        let path = parts[..parts.len() - 1]
            .iter()
            .map(|&s| s.trim().to_string())
            .collect();
        return path;
    }

    fn search_json(&self, path: &[String], partial_key: &str) -> Vec<(String, Option<String>)> {
        let mut current = &self.completion_json;
        for key in path {
            if let Some(value) = current.get(key) {
                current = value;
            } else {
                return Vec::new();
            }
        }

        match current {
            Value::Object(map) => map
                .iter()
                .filter(|(k, _)| k.starts_with(partial_key))
                .map(|(k, v)| {
                    let description = match v {
                        Value::Object(obj) => obj
                            .get("description")
                            .and_then(|d| d.as_str())
                            .map(String::from),
                        _ => None,
                    };
                    (k.clone(), description)
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, _: InitializeParams) -> Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: None,
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions {
                    resolve_provider: Some(false),
                    trigger_characters: Some(vec![".".to_string()]),
                    work_done_progress_options: Default::default(),
                    all_commit_characters: None,
                    ..Default::default()
                }),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec!["dummy.do_something".to_string()],
                    work_done_progress_options: Default::default(),
                }),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: Some(WorkspaceFoldersServerCapabilities {
                        supported: Some(true),
                        change_notifications: Some(OneOf::Left(true)),
                    }),
                    file_operations: None,
                }),
                ..ServerCapabilities::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "devenv lsp is now initialized!")
            .await;
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_change_workspace_folders(&self, _: DidChangeWorkspaceFoldersParams) {
        self.client
            .log_message(MessageType::INFO, "workspace folders changed!")
            .await;
    }

    async fn did_change_configuration(&self, _: DidChangeConfigurationParams) {
        self.client
            .log_message(MessageType::INFO, "configuration changed!")
            .await;
    }

    async fn did_change_watched_files(&self, _: DidChangeWatchedFilesParams) {
        self.client
            .log_message(MessageType::INFO, "watched files have changed!")
            .await;
    }

    async fn execute_command(&self, _: ExecuteCommandParams) -> Result<Option<Value>> {
        self.client
            .log_message(MessageType::INFO, "command executed!")
            .await;

        match self.client.apply_edit(WorkspaceEdit::default()).await {
            Ok(res) if res.applied => self.client.log_message(MessageType::INFO, "applied").await,
            Ok(_) => self.client.log_message(MessageType::INFO, "rejected").await,
            Err(err) => self.client.log_message(MessageType::ERROR, err).await,
        }

        Ok(None)
    }

    async fn did_open(&self, _: DidOpenTextDocumentParams) {
        self.client
            .log_message(MessageType::INFO, "file opened!")
            .await;
        info!("textDocument/DidOpen");
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        // info!("textDocument/DidChange, params: {:?}", params);
        let uri = params.text_document.uri.to_string();

        // Get the last known cursor position for this document
        let position = self
            .last_cursor_position
            .get(&uri)
            .map(|pos| *pos)
            .unwrap_or_default();

        let line = position.line as usize;
        let character = position.character as usize;
        let file_content = params.content_changes[0].text.clone();
        self.document_map.insert(uri.clone(), file_content.clone());

        let mut parser = Parser::new();
        let nix_grammer = language();
        parser
            .set_language(nix_grammer)
            .expect("Error loading Nix grammar");

        let tree = parser
            .parse(&file_content, None)
            .expect("Failed to parse document");

        let root_node = tree.root_node();
        let point: Point = Point::new(line as usize, character as usize);
        let scope_path = self.get_scope(root_node, point, &file_content);
        self.current_scope.insert(uri, scope_path);

        self.client
            .log_message(MessageType::INFO, "file changed!")
            .await;
    }

    async fn did_save(&self, _: DidSaveTextDocumentParams) {
        info!("textDocument/DidSave");
        self.client
            .log_message(MessageType::INFO, "file saved!")
            .await;
    }

    async fn did_close(&self, _: DidCloseTextDocumentParams) {
        info!("textDocument/DidClose");
        self.client
            .log_message(MessageType::INFO, "file closed!")
            .await;
    }

    async fn completion(&self, params: CompletionParams) -> Result<Option<CompletionResponse>> {
        info!("textDocument/Completion");
        let uri = params.text_document_position.text_document.uri.to_string();

        let file_content = match self.document_map.get(uri.as_str()) {
            Some(content) => {
                debug!("Text document content via DashMap: {:?}", content.deref());
                content.clone()
            }
            None => {
                info!("No content found for the given URI");
                String::new()
            }
        };

        let position = params.text_document_position.position;
        let line = position.line as usize;
        let character = position.character as usize;

        let line_content = file_content.lines().nth(line).unwrap_or_default();
        let line_until_cursor = &line_content[..character];

        self.last_cursor_position.insert(uri.clone(), position);

        // let tree = self.parse_document(&file_content);

        // let root_node = tree.root_node();

        // let point: Point = Point::new(line as usize, character as usize);

        // let scope_path = self.get_scope(root_node, point, &file_content);

        let re = Regex::new(r".*\W(.*)").unwrap(); // Define the regex pattern
        let current_word = re
            .captures(line_until_cursor)
            .and_then(|caps| caps.get(1))
            .map(|m| m.as_str())
            .unwrap_or("");

        debug!("Current scope {:?}", self.current_scope);
        debug!("Line until cursor: {:?}", line_until_cursor);

        let dot_path = self.get_path(line_until_cursor);
        let current_scope = self
            .current_scope
            .get(uri.as_str())
            .map(|ref_wrapper| ref_wrapper.clone()) // Clone the inner Vec<String>
            .unwrap_or_else(Vec::new); // If there's no scope, use an empty Vec

        let search_path = [current_scope, dot_path].concat();

        debug!("Path: {:?}, Partial key: {:?}", search_path, current_word);

        let completions = self.search_json(&search_path, &current_word);

        info!(
            "Probable completion items {:?} and description",
            completions
        );

        let completion_items: Vec<CompletionItem> = completions
            .into_iter()
            .map(|(item, description)| {
                CompletionItem::new_simple(item, description.unwrap_or_default())
            })
            .collect();

        Ok(Some(CompletionResponse::List(CompletionList {
            is_incomplete: false,
            items: completion_items,
        })))
    }
}

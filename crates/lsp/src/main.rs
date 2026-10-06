//! Language server of Mollie: diagnostics, hover, go to definition and
//! completion.
//!
//! Every open document is type-checked whenever it changes: as the root module
//! of a program, with its submodules read from open documents or from disk,
//! or, in a project (`mollie.toml`), as a submodule of the entry whose
//! program contains it. The stub of the host's API (`.mollie/host`, see
//! `mollie::stub`) is loaded first, so the host's functions and types are
//! known.

mod analysis;
mod features;

use std::{collections::HashMap, error::Error};

use lsp_server::{Connection, ErrorCode, Message, Notification as ServerNotification, Request as ServerRequest, RequestId, Response};
use lsp_types::{
    CompletionOptions, CompletionRequest, CompletionResponse, Definition, DefinitionProvider, DefinitionRequest, DefinitionResponse,
    DidChangeTextDocumentNotification, DidCloseTextDocumentNotification, DidOpenTextDocumentNotification, HoverProvider, HoverRequest, InitializeParams,
    InitializeResult, Notification, PublishDiagnosticsNotification, PublishDiagnosticsParams, Request, ServerCapabilities, ServerInfo,
    TextDocumentContentChangeEvent, TextDocumentSync, TextDocumentSyncKind, Uri,
};

use crate::analysis::{Analysis, Documents, Encoding, Project};

struct Server {
    connection: Connection,
    encoding: Encoding,
    documents: Documents,
    /// The latest analysis of every open document.
    analyses: HashMap<Uri, Analysis>,
    /// Files with published diagnostics, by the document whose analysis
    /// published them, to clear them later.
    published: HashMap<Uri, Vec<Uri>>,
}

fn main() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();
    let (id, params) = connection.initialize_start()?;
    let params: InitializeParams = serde_json::from_value(params)?;
    let offered = params.capabilities.general.and_then(|general| general.position_encodings).unwrap_or_default();
    let encoding = Encoding::negotiate(&offered);

    let capabilities = ServerCapabilities {
        position_encoding: Some(encoding.kind()),
        text_document_sync: Some(TextDocumentSync::Kind(TextDocumentSyncKind::Full)),
        hover_provider: Some(HoverProvider::Bool(true)),
        definition_provider: Some(DefinitionProvider::Bool(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![String::from(".")]),
            ..CompletionOptions::default()
        }),
        ..ServerCapabilities::default()
    };

    let result = InitializeResult {
        capabilities,
        server_info: Some(ServerInfo::new(String::from("mollie-lsp"), Some(String::from(env!("CARGO_PKG_VERSION"))))),
    };

    connection.initialize_finish(id, serde_json::to_value(result)?)?;

    let mut server = Server {
        connection,
        encoding,
        documents: Documents::default(),
        analyses: HashMap::new(),
        published: HashMap::new(),
    };

    server.run()?;

    drop(server);
    io_threads.join()?;

    Ok(())
}

impl Server {
    fn run(&mut self) -> Result<(), Box<dyn Error + Sync + Send>> {
        while let Ok(message) = self.connection.receiver.recv() {
            match message {
                Message::Request(request) => {
                    if self.connection.handle_shutdown(&request)? {
                        return Ok(());
                    }

                    let response = self.handle_request(request);

                    self.connection.sender.send(response.into())?;
                }
                Message::Notification(notification) => self.handle_notification(notification)?,
                Message::Response(_) => (),
            }
        }

        Ok(())
    }

    fn handle_request(&mut self, request: ServerRequest) -> Response {
        let id = request.id.clone();
        let method = request.method.clone();

        match method.as_str() {
            method if method == HoverRequest::METHOD.as_str() => self.respond::<HoverRequest>(request, |server, params| {
                let position = params.text_document_position_params;
                let uri = position.text_document.uri;
                let analysis = server.analyses.get_mut(&uri)?;
                let module = analysis.module_of(&uri)?;
                let offset = analysis.files.get(&module)?.index.offset(position.position);

                features::hover(analysis, module, offset)
            }),
            method if method == DefinitionRequest::METHOD.as_str() => self.respond::<DefinitionRequest>(request, |server, params| {
                let position = params.text_document_position_params;
                let uri = position.text_document.uri;
                let analysis = server.analyses.get(&uri)?;
                let module = analysis.module_of(&uri)?;
                let offset = analysis.files.get(&module)?.index.offset(position.position);

                features::definition(analysis, module, offset).map(|location| DefinitionResponse::Definition(Definition::Location(location)))
            }),
            method if method == CompletionRequest::METHOD.as_str() => self.respond::<CompletionRequest>(request, |server, params| {
                let position = params.text_document_position_params;
                let uri = position.text_document.uri;
                let text = server.documents.get(&uri)?.to_owned();
                let offset = analysis::LineIndex::new(text.clone(), server.encoding).offset(position.position);
                let project = project_of(&uri);
                let root = server.root_of(&uri, project.as_ref());
                let items = features::completion(&root, &uri, &text, offset, &server.documents, server.encoding, project.as_ref());

                Some(CompletionResponse::CompletionItemList(items))
            }),
            _ => Response::new_err(id, ErrorCode::MethodNotFound as i32, format!("unsupported request `{method}`")),
        }
    }

    /// Responds to a request of type `R` with the result of `handle`.
    fn respond<R: Request>(&mut self, request: ServerRequest, handle: impl FnOnce(&mut Self, R::Params) -> R::Result) -> Response {
        let id: RequestId = request.id.clone();

        match request.extract::<R::Params>(R::METHOD.as_str()) {
            Ok((_, params)) => Response::new_ok(id, handle(self, params)),
            Err(error) => Response::new_err(id, ErrorCode::InvalidParams as i32, error.to_string()),
        }
    }

    fn handle_notification(&mut self, notification: ServerNotification) -> Result<(), Box<dyn Error + Sync + Send>> {
        let method = notification.method.clone();

        match method.as_str() {
            method if method == DidOpenTextDocumentNotification::METHOD.as_str() => {
                // Malformed notifications are ignored.
                let Ok(params) = notification.extract::<<DidOpenTextDocumentNotification as Notification>::Params>(method) else {
                    return Ok(());
                };
                let document = params.text_document;

                self.documents.set(document.uri.clone(), document.text);
                self.analyze(&document.uri)?;
            }
            method if method == DidChangeTextDocumentNotification::METHOD.as_str() => {
                let Ok(params) = notification.extract::<<DidChangeTextDocumentNotification as Notification>::Params>(method) else {
                    return Ok(());
                };
                let uri = params.text_document.text_document_identifier.uri;

                // Documents are synced in full, so the last change is the text.
                if let Some(TextDocumentContentChangeEvent::TextDocumentContentChangeWholeDocument(change)) = params.content_changes.into_iter().last() {
                    self.documents.set(uri.clone(), change.text);
                }

                // Documents importing this one see the change too.
                let open = self.analyses.keys().cloned().collect::<Vec<_>>();

                for document in open {
                    if document == uri || self.analyses[&document].files.values().any(|file| file.uri == uri) {
                        self.analyze(&document)?;
                    }
                }

                if !self.analyses.contains_key(&uri) {
                    self.analyze(&uri)?;
                }
            }
            method if method == DidCloseTextDocumentNotification::METHOD.as_str() => {
                let Ok(params) = notification.extract::<<DidCloseTextDocumentNotification as Notification>::Params>(method) else {
                    return Ok(());
                };
                let uri = params.text_document.uri;

                self.documents.remove(&uri);
                self.analyses.remove(&uri);

                for file in self.published.remove(&uri).unwrap_or_default() {
                    self.publish(file, Vec::new())?;
                }
            }
            _ => (),
        }

        Ok(())
    }

    /// The root module of the program the document `uri` belongs to: the
    /// entry of `project` whose program contains it, or the document itself.
    fn root_of(&self, uri: &Uri, project: Option<&Project>) -> Uri {
        let (Some(project), Ok(path)) = (project, uri.to_file_path()) else {
            return uri.clone();
        };

        if project.entries.contains(&path) {
            return uri.clone();
        }

        for entry in &project.entries {
            let (Ok(entry_uri), Some(text)) = (Uri::from_file_path(entry), self.documents.read(entry)) else {
                continue;
            };

            if Analysis::try_new(&entry_uri, text, &self.documents, self.encoding, Some(project)).is_some_and(|analysis| analysis.module_of(uri).is_some()) {
                return entry_uri;
            }
        }

        uri.clone()
    }

    /// Type-checks the program of the open document `uri` and publishes
    /// diagnostics of its files.
    fn analyze(&mut self, uri: &Uri) -> Result<(), Box<dyn Error + Sync + Send>> {
        let project = project_of(uri);
        let root = self.root_of(uri, project.as_ref());
        let text = if root == *uri {
            self.documents.get(uri).map(str::to_owned)
        } else {
            root.to_file_path().ok().and_then(|path| self.documents.read(&path))
        };
        let Some(text) = text else {
            return Ok(());
        };

        // The previous analysis is kept if this one fails.
        let Some(analysis) = Analysis::try_new(&root, text, &self.documents, self.encoding, project.as_ref()) else {
            return Ok(());
        };
        let diagnostics = features::diagnostics(&analysis);
        let files = diagnostics.keys().cloned().collect::<Vec<_>>();

        // Files that no longer belong to the program lose their diagnostics.
        for file in self.published.remove(uri).unwrap_or_default() {
            if !files.contains(&file) {
                self.publish(file, Vec::new())?;
            }
        }

        for (file, diagnostics) in diagnostics {
            self.publish(file, diagnostics)?;
        }

        self.published.insert(uri.clone(), files);
        self.analyses.insert(uri.clone(), analysis);

        Ok(())
    }

    fn publish(&self, uri: Uri, diagnostics: Vec<lsp_types::Diagnostic>) -> Result<(), Box<dyn Error + Sync + Send>> {
        let params = PublishDiagnosticsParams::new(uri, None, diagnostics);
        let notification = ServerNotification::new(PublishDiagnosticsNotification::METHOD.as_str().to_owned(), params);

        self.connection.sender.send(notification.into())?;

        Ok(())
    }
}

/// The project of the document `uri`, if it's in one.
fn project_of(uri: &Uri) -> Option<Project> {
    uri.to_file_path().ok().and_then(|path| Project::find(&path))
}

import * as cp from "child_process";
import * as path from "path";
import * as vscode from "vscode";
import {
  DocumentFilter,
  DocumentSelector,
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
} from "vscode-languageclient/node";

const CLIENT_ID = "remark-lsp";
const TASK_TYPE = "remark";
const TASK_OPEN_DRAFT = "openDraft";
const COMMAND_ADD_LINE = "remark.addLineComment";
const COMMAND_ADD_FILE = "remark.addFileComment";
const COMMAND_CREATE_COMMENT = "remark.createComment";
const COMMAND_RESOLVE_COMMENT = "remark.resolveComment";
const SERVER_DRAFT_COMMAND = "remark.addDraftComment";
const COMMENT_CONTROLLER_ID = "remark";
const DEFAULT_LANGUAGES = ["*"];
const COMMENT_AUTHOR: vscode.CommentAuthorInformation = { name: "remark" };

const clients = new Map<string, LanguageClient>();
let outputChannel: vscode.OutputChannel | undefined;
let commentController: vscode.CommentController | undefined;
const commentThreads = new Map<string, vscode.CommentThread>();
type ThreadMeta = {
  key?: string;
  pendingUntil?: number;
  isFileComment?: boolean;
  line?: number;
  side?: "old" | "new";
};
const commentThreadMeta = new Map<vscode.CommentThread, ThreadMeta>();

export async function activate(context: vscode.ExtensionContext): Promise<void> {
  if (!outputChannel) {
    outputChannel = vscode.window.createOutputChannel("remark-lsp");
    context.subscriptions.push(outputChannel);
  }
  if (!commentController) {
    commentController = vscode.comments.createCommentController(
      COMMENT_CONTROLLER_ID,
      "Remark"
    );
    commentController.options = {
      prompt: "Add remark comment",
      placeHolder: "Write a review comment...",
    };
    commentController.commentingRangeProvider = {
      provideCommentingRanges: (document: vscode.TextDocument) => {
        if (!isLanguageEnabled(document.languageId)) {
          return [];
        }
        if (document.lineCount === 0) {
          return [];
        }
        return [new vscode.Range(0, 0, document.lineCount - 1, 0)];
      },
    };
    context.subscriptions.push(commentController);
  }
  context.subscriptions.push(
    vscode.commands.registerCommand(
      COMMAND_CREATE_COMMENT,
      async (reply: vscode.CommentReply) => {
        await handleCommentReply(reply);
      }
    ),
    vscode.commands.registerCommand(
      COMMAND_RESOLVE_COMMENT,
      async (thread: vscode.CommentThread) => {
        await resolveCommentThread(thread);
      }
    ),
    vscode.commands.registerCommand("remark.openDraft", async () => {
      await openDraftCommand();
    }),
    vscode.commands.registerCommand("remark.restartLsp", async () => {
      await restartAllClients();
    }),
    vscode.commands.registerCommand(COMMAND_ADD_LINE, async (uri?: vscode.Uri, line?: number) => {
      await addLineCommentCommand(uri, line);
    }),
    vscode.commands.registerCommand(COMMAND_ADD_FILE, async (uri?: vscode.Uri) => {
      await addFileCommentCommand(uri);
    }),
    vscode.languages.registerCodeActionsProvider(
      { scheme: "file" },
      new RemarkCodeActionProvider(),
      { providedCodeActionKinds: [vscode.CodeActionKind.QuickFix] }
    ),
    vscode.tasks.registerTaskProvider(TASK_TYPE, new RemarkTaskProvider())
  );

  await startClientsForWorkspace();
  refreshCommentThreadsForOpenDocuments();

  context.subscriptions.push(
    vscode.languages.onDidChangeDiagnostics((event) => {
      for (const uri of event.uris) {
        const document = vscode.workspace.textDocuments.find(
          (doc) => doc.uri.toString() === uri.toString()
        );
        if (document) {
          refreshCommentThreadsForDocument(document);
        }
      }
    }),
    vscode.workspace.onDidOpenTextDocument((document) => {
      refreshCommentThreadsForDocument(document);
    }),
    vscode.workspace.onDidCloseTextDocument((document) => {
      disposeCommentThreadsForUri(document.uri);
    }),
    vscode.workspace.onDidChangeWorkspaceFolders(async (event) => {
      for (const folder of event.added) {
        await startClientForFolder(folder);
      }
      for (const folder of event.removed) {
        await stopClientForFolder(folder);
      }
    }),
    vscode.workspace.onDidChangeConfiguration(async (event) => {
      if (event.affectsConfiguration("remark")) {
        await restartAllClients();
      }
    })
  );
}

export async function deactivate(): Promise<void> {
  await stopAllClients();
}

async function startClientsForWorkspace(): Promise<void> {
  const folders = vscode.workspace.workspaceFolders ?? [];
  await Promise.all(folders.map((folder) => startClientForFolder(folder)));
}

async function startClientForFolder(
  folder: vscode.WorkspaceFolder
): Promise<void> {
  const key = folder.uri.toString();
  if (clients.has(key)) {
    return;
  }

  const serverOptions = getServerOptions(folder);
  const clientOptions = getClientOptions(folder);

  const client = new LanguageClient(
    `${CLIENT_ID}:${folder.name}`,
    "remark-lsp",
    serverOptions,
    clientOptions
  );

  clients.set(key, client);
  await client.start();
}

async function stopClientForFolder(
  folder: vscode.WorkspaceFolder
): Promise<void> {
  const key = folder.uri.toString();
  const client = clients.get(key);
  if (!client) {
    return;
  }
  clients.delete(key);
  await client.stop();
}

async function stopAllClients(): Promise<void> {
  const stopPromises = Array.from(clients.values()).map((client) =>
    client.stop()
  );
  clients.clear();
  await Promise.allSettled(stopPromises);
}

async function restartAllClients(): Promise<void> {
  await stopAllClients();
  await startClientsForWorkspace();
}

function getServerOptions(folder: vscode.WorkspaceFolder): ServerOptions {
  const command = resolveRemarkCommand();
  const args = resolveLspArgs();
  return {
    command,
    args,
    options: {
      cwd: folder.uri.fsPath,
      env: process.env,
    },
  };
}

function getClientOptions(
  folder: vscode.WorkspaceFolder
): LanguageClientOptions {
  return {
    documentSelector: getDocumentSelector(folder),
    workspaceFolder: folder,
    outputChannel: outputChannel ?? vscode.window.createOutputChannel("remark-lsp"),
    middleware: {
      window: {
        showDocument: async (params, next) => {
          const uri = vscode.Uri.parse(params.uri);
          if (isDraftUri(uri)) {
            await openDraftPath(uri.fsPath);
            return { success: true };
          }
          const cancelSource = new vscode.CancellationTokenSource();
          const result = await next(params, cancelSource.token);
          cancelSource.dispose();
          if ("success" in result) {
            return result;
          }
          return { success: false };
        },
      },
      provideCodeActions: async (document, range, context, token, next) => {
        const actions = await next(document, range, context, token);
        return filterDraftCodeActions(actions);
      },
    },
  };
}

function getDocumentSelector(
  folder: vscode.WorkspaceFolder
): DocumentSelector {
  const configured = getConfiguredLanguages();
  const folderPattern = toGlobPattern(folder, "**/*");
  const selector: DocumentFilter[] = [];

  if (configured.length === 1 && configured[0] === "*") {
    selector.push({ scheme: "file", pattern: folderPattern });
  } else {
    for (const language of configured) {
      selector.push({ scheme: "file", language, pattern: folderPattern });
    }
  }

  selector.push({
    scheme: "file",
    pattern: toGlobPattern(folder, ".git/remark/draft.md"),
  });
  return selector;
}

function filterDraftCodeActions(
  actions:
    | (vscode.Command | vscode.CodeAction)[]
    | null
    | undefined
): (vscode.Command | vscode.CodeAction)[] | null | undefined {
  if (!actions) {
    return actions;
  }
  const filtered = actions.filter((action) => !isDraftCommandAction(action));
  return filtered;
}

function refreshCommentThreadsForOpenDocuments(): void {
  for (const document of vscode.workspace.textDocuments) {
    refreshCommentThreadsForDocument(document);
  }
}

function refreshCommentThreadsForDocument(document: vscode.TextDocument): void {
  if (!commentController) {
    return;
  }
  if (document.uri.scheme !== "file") {
    return;
  }
  if (!isLanguageEnabled(document.languageId)) {
    disposeCommentThreadsForUri(document.uri);
    return;
  }

  const diagnostics = vscode.languages.getDiagnostics(document.uri);
  const now = Date.now();
  const nextKeys = new Set<string>();

  for (const diagnostic of diagnostics) {
    if (!isRemarkDiagnostic(diagnostic)) {
      continue;
    }
    const parsed = parseRemarkDiagnostic(diagnostic);
    const range = makeThreadRange(diagnostic.range.start.line);
    const key = makeThreadKey(document.uri, range, parsed.message, parsed.side, parsed.isFileComment);
    nextKeys.add(key);

    let thread = commentThreads.get(key);
    if (!thread) {
      thread = commentController.createCommentThread(document.uri, range, []);
      commentThreads.set(key, thread);
    }

    thread.range = range;
    thread.comments = [buildComment(parsed.message)];
    thread.canReply = false;
    thread.collapsibleState = vscode.CommentThreadCollapsibleState.Collapsed;
    thread.state = vscode.CommentThreadState.Unresolved;
    thread.contextValue = "remarkExisting";
    commentThreadMeta.set(thread, {
      key,
      isFileComment: parsed.isFileComment,
      line: parsed.line,
      side: parsed.side,
    });
  }

  const prefix = commentThreadKeyPrefix(document.uri);
  for (const [key, thread] of commentThreads) {
    if (!key.startsWith(prefix)) {
      continue;
    }
    if (nextKeys.has(key)) {
      continue;
    }
    const meta = commentThreadMeta.get(thread);
    if (meta?.pendingUntil && meta.pendingUntil > now) {
      continue;
    }
    thread.dispose();
    commentThreads.delete(key);
    commentThreadMeta.delete(thread);
  }
}

function disposeCommentThreadsForUri(uri: vscode.Uri): void {
  const prefix = commentThreadKeyPrefix(uri);
  for (const [key, thread] of commentThreads) {
    if (!key.startsWith(prefix)) {
      continue;
    }
    thread.dispose();
    commentThreads.delete(key);
    commentThreadMeta.delete(thread);
  }

  for (const [thread, meta] of commentThreadMeta) {
    if (meta.key) {
      continue;
    }
    if (thread.uri.toString() !== uri.toString()) {
      continue;
    }
    thread.dispose();
    commentThreadMeta.delete(thread);
  }
}

function isRemarkDiagnostic(diagnostic: vscode.Diagnostic): boolean {
  return diagnostic.source === "remark";
}

function normalizeDiagnosticMessage(message: string): string {
  let text = message.trim();
  if (text.startsWith("[old] ")) {
    text = text.slice(6);
  }
  text = text.replace(/\s+\(line\s+\d+\)$/, "");
  return text;
}

function parseRemarkDiagnostic(diagnostic: vscode.Diagnostic): {
  message: string;
  isFileComment: boolean;
  line: number;
  side: "old" | "new";
} {
  const raw = diagnostic.message.trim();
  const side: "old" | "new" = raw.startsWith("[old] ") ? "old" : "new";
  const isFileComment = !/\(line\s+\d+\)$/.test(raw);
  const message = normalizeDiagnosticMessage(raw);
  return {
    message,
    isFileComment,
    line: diagnostic.range.start.line + 1,
    side,
  };
}

function makeThreadRange(line: number): vscode.Range {
  return new vscode.Range(line, 0, line, 0);
}

function makeThreadKey(
  uri: vscode.Uri,
  range: vscode.Range | undefined,
  message: string,
  side?: "old" | "new",
  isFileComment?: boolean
): string {
  const rangeKey = range ? `${range.start.line}:${range.start.character}` : "file";
  const sideKey = side ? `:${side}` : "";
  const kindKey = isFileComment ? ":file" : "";
  return `${uri.toString()}|${rangeKey}${sideKey}${kindKey}|${message}`;
}

function commentThreadKeyPrefix(uri: vscode.Uri): string {
  return `${uri.toString()}|`;
}

function buildComment(body: string): vscode.Comment {
  return {
    body,
    mode: vscode.CommentMode.Preview,
    author: COMMENT_AUTHOR,
  };
}

function isDraftCommandAction(action: vscode.Command | vscode.CodeAction): boolean {
  if (isCommand(action)) {
    return action.command === SERVER_DRAFT_COMMAND;
  }
  const command = action.command?.command;
  return command === SERVER_DRAFT_COMMAND;
}

function isCommand(
  action: vscode.Command | vscode.CodeAction
): action is vscode.Command {
  return typeof (action as vscode.Command).command === "string";
}

class RemarkCodeActionProvider implements vscode.CodeActionProvider {
  provideCodeActions(
    document: vscode.TextDocument,
    range: vscode.Range
  ): vscode.CodeAction[] | undefined {
    if (!isLanguageEnabled(document.languageId)) {
      return undefined;
    }
    const line = range.start.line + 1;
    const lineAction = new vscode.CodeAction(
      "Remark: Add line comment",
      vscode.CodeActionKind.QuickFix
    );
    lineAction.command = {
      command: COMMAND_ADD_LINE,
      title: lineAction.title,
      arguments: [document.uri, line],
    };

    const fileAction = new vscode.CodeAction(
      "Remark: Add file comment",
      vscode.CodeActionKind.QuickFix
    );
    fileAction.command = {
      command: COMMAND_ADD_FILE,
      title: fileAction.title,
      arguments: [document.uri],
    };

    return [lineAction, fileAction];
  }
}

function isLanguageEnabled(languageId: string): boolean {
  const languages = getConfiguredLanguages();
  if (languages.includes("*")) {
    return true;
  }
  return languages.includes(languageId);
}

function getConfiguredLanguages(): string[] {
  const config = vscode.workspace.getConfiguration("remark");
  const languages = config.get<string[]>("languages");
  if (languages && languages.length > 0) {
    return languages;
  }
  return DEFAULT_LANGUAGES;
}

function resolveRemarkCommand(): string {
  const config = vscode.workspace.getConfiguration("remark");
  const configured = config.get<string>("path");
  if (configured && configured.trim().length > 0) {
    return configured.trim();
  }
  return "remark";
}

function resolveLspArgs(): string[] {
  const envArgs = splitArgs(process.env.REMARK_LSP_ARGS ?? "");
  const configArgs = getConfigArgs();
  return ["lsp", ...envArgs, ...configArgs];
}

function getConfigArgs(): string[] {
  const config = vscode.workspace.getConfiguration("remark");
  const raw = config.get<string | string[]>("lspArgs");
  if (!raw) {
    return [];
  }
  if (Array.isArray(raw)) {
    return raw.map((item) => item.trim()).filter((item) => item.length > 0);
  }
  return splitArgs(raw);
}

function splitArgs(value: string): string[] {
  return value
    .split(/\s+/)
    .map((item) => item.trim())
    .filter((item) => item.length > 0);
}

function toWorkspaceRelativePath(
  folder: vscode.WorkspaceFolder,
  absolutePath: string
): string | undefined {
  const rel = path.relative(folder.uri.fsPath, absolutePath);
  if (!rel || rel.startsWith("..") || path.isAbsolute(rel)) {
    return undefined;
  }
  return rel.replace(/\\/g, "/");
}

function toGlobPattern(folder: vscode.WorkspaceFolder, suffix: string): string {
  const base = folder.uri.fsPath.replace(/\\\\/g, "/");
  return `${base}/${suffix}`;
}

function isDraftUri(uri: vscode.Uri): boolean {
  const normalized = uri.fsPath.replace(/\\/g, "/");
  return normalized.endsWith("/remark/draft.md");
}

async function openDraftCommand(): Promise<void> {
  const folder = await pickWorkspaceFolder();
  if (!folder) {
    vscode.window.showErrorMessage(
      "Remark: open draft requires an open workspace folder."
    );
    return;
  }
  await openDraftForWorkspace(folder);
}

async function openDraftForWorkspace(
  folder: vscode.WorkspaceFolder
): Promise<void> {
  try {
    const draftPath = await resolveDraftPath(folder);
    await openDraftPath(draftPath);
  } catch (err) {
    const message = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Remark: ${message}`);
  }
}

async function openDraftPath(draftPath: string): Promise<void> {
  const draftUri = vscode.Uri.file(draftPath);
  const editor = await showDraftEditor(draftUri);
  if (!editor) {
    return;
  }
  await handleDirtyDraft(editor, draftUri);
}

function findVisibleDraftEditor(
  draftUri: vscode.Uri
): vscode.TextEditor | undefined {
  return vscode.window.visibleTextEditors.find(
    (editor) => editor.document.uri.toString() === draftUri.toString()
  );
}

function findDraftTabColumn(draftUri: vscode.Uri): vscode.ViewColumn | undefined {
  for (const group of vscode.window.tabGroups.all) {
    for (const tab of group.tabs) {
      const input = tab.input;
      if (
        input instanceof vscode.TabInputText &&
        input.uri.toString() === draftUri.toString()
      ) {
        return group.viewColumn;
      }
    }
  }
  return undefined;
}

function pickNonActiveColumn(
  exclude?: vscode.ViewColumn
): vscode.ViewColumn {
  const active = vscode.window.activeTextEditor;
  const activeColumn = active?.viewColumn;
  const otherEditor = vscode.window.visibleTextEditors.find(
    (editor) =>
      editor.viewColumn !== undefined &&
      editor.viewColumn !== activeColumn &&
      editor.viewColumn !== exclude
  );
  if (otherEditor?.viewColumn) {
    return otherEditor.viewColumn;
  }
  return vscode.ViewColumn.Beside;
}

async function showDraftEditor(
  draftUri: vscode.Uri
): Promise<vscode.TextEditor | undefined> {
  const visibleDraft = findVisibleDraftEditor(draftUri);
  if (visibleDraft) {
    return vscode.window.showTextDocument(visibleDraft.document, {
      viewColumn: visibleDraft.viewColumn,
      preview: false,
    });
  }

  const existingColumn = findDraftTabColumn(draftUri);
  if (existingColumn) {
    const doc = await vscode.workspace.openTextDocument(draftUri);
    return vscode.window.showTextDocument(doc, {
      viewColumn: existingColumn,
      preview: false,
    });
  }

  const targetColumn = pickNonActiveColumn();
  const doc = await vscode.workspace.openTextDocument(draftUri);
  return vscode.window.showTextDocument(doc, {
    viewColumn: targetColumn,
    preview: false,
  });
}

async function handleDirtyDraft(
  editor: vscode.TextEditor,
  draftUri: vscode.Uri
): Promise<void> {
  if (!editor.document.isDirty) {
    return;
  }

  const choice = await vscode.window.showWarningMessage(
    "Remark: draft has unsaved changes.",
    { modal: true },
    "Reload from disk",
    "Open disk snapshot"
  );

  if (choice === "Reload from disk") {
    await revertEditor(editor);
    return;
  }

  await openDraftSnapshot(editor, draftUri);
}

async function revertEditor(editor: vscode.TextEditor): Promise<void> {
  await vscode.window.showTextDocument(editor.document, {
    viewColumn: editor.viewColumn,
    preview: false,
  });
  await vscode.commands.executeCommand("workbench.action.files.revert");
}

async function openDraftSnapshot(
  editor: vscode.TextEditor,
  draftUri: vscode.Uri
): Promise<void> {
  const bytes = await vscode.workspace.fs.readFile(draftUri);
  const content = new TextDecoder("utf-8").decode(bytes);
  const snapshotDoc = await vscode.workspace.openTextDocument({
    content,
    language: "markdown",
  });
  const targetColumn = pickNonActiveColumn(editor.viewColumn);
  await vscode.window.showTextDocument(snapshotDoc, {
    viewColumn: targetColumn,
    preview: false,
  });
}

type TargetDocument = {
  uri: vscode.Uri;
  folder: vscode.WorkspaceFolder;
  relPath: string;
  line?: number;
};

function resolveTargetDocument(uri?: vscode.Uri): TargetDocument | undefined {
  const editor = vscode.window.activeTextEditor;
  let targetUri = uri;
  let line: number | undefined;

  if (!targetUri && editor) {
    targetUri = editor.document.uri;
    line = editor.selection.active.line + 1;
  } else if (targetUri && editor && editor.document.uri.toString() === targetUri.toString()) {
    line = editor.selection.active.line + 1;
  }

  if (!targetUri) {
    vscode.window.showErrorMessage("Remark: open a file to add a comment.");
    return undefined;
  }
  if (targetUri.scheme !== "file") {
    vscode.window.showErrorMessage("Remark: only file-backed documents are supported.");
    return undefined;
  }

  const folder = vscode.workspace.getWorkspaceFolder(targetUri);
  if (!folder) {
    vscode.window.showErrorMessage(
      "Remark: file must be inside an open workspace folder."
    );
    return undefined;
  }

  const relPath = toWorkspaceRelativePath(folder, targetUri.fsPath);
  if (!relPath) {
    vscode.window.showErrorMessage("Remark: file is outside the workspace.");
    return undefined;
  }

  return { uri: targetUri, folder, relPath, line };
}

async function handleCommentReply(reply: vscode.CommentReply): Promise<void> {
  const text = String(reply.text ?? "").trim();
  if (!text) {
    vscode.window.showErrorMessage("Remark: comment text cannot be empty.");
    return;
  }

  const thread = reply.thread;
  const uri = thread.uri;
  if (uri.scheme !== "file") {
    vscode.window.showErrorMessage("Remark: only file-backed documents are supported.");
    return;
  }

  const folder = vscode.workspace.getWorkspaceFolder(uri);
  if (!folder) {
    vscode.window.showErrorMessage(
      "Remark: file must be inside an open workspace folder."
    );
    return;
  }

  const relPath = toWorkspaceRelativePath(folder, uri.fsPath);
  if (!relPath) {
    vscode.window.showErrorMessage("Remark: file is outside the workspace.");
    return;
  }

  const meta = commentThreadMeta.get(thread);
  const isFileComment = meta?.isFileComment ?? false;
  const lineNumber = thread.range ? thread.range.start.line + 1 : 1;

  try {
    await runRemarkAdd(folder, {
      file: relPath,
      line: lineNumber,
      side: "new",
      fileComment: isFileComment,
      message: text,
    });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Remark: ${msg}`);
    return;
  }

  const comment = buildComment(text);
  thread.comments = [comment];
  thread.canReply = false;
  thread.collapsibleState = vscode.CommentThreadCollapsibleState.Collapsed;
  thread.state = vscode.CommentThreadState.Unresolved;
  thread.contextValue = "remarkExisting";

  const range = thread.range ?? makeThreadRange(0);
  thread.range = range;
  const key = makeThreadKey(uri, range, text, "new", isFileComment);
  commentThreads.set(key, thread);
  commentThreadMeta.set(thread, {
    key,
    isFileComment,
    line: lineNumber,
    side: "new",
    pendingUntil: Date.now() + 5000,
  });
}

async function resolveCommentThread(thread: vscode.CommentThread): Promise<void> {
  const args = buildThreadCommandArgs(thread);
  if (!args) {
    return;
  }
  await executeLspCommand(thread.uri, "remark.resolve", args);
}

function buildThreadCommandArgs(
  thread: vscode.CommentThread
): { file: string; line?: number; side?: string; file_comment: boolean } | undefined {
  const uri = thread.uri;
  if (uri.scheme !== "file") {
    vscode.window.showErrorMessage("Remark: only file-backed comments are supported.");
    return undefined;
  }
  const folder = vscode.workspace.getWorkspaceFolder(uri);
  if (!folder) {
    vscode.window.showErrorMessage(
      "Remark: file must be inside an open workspace folder."
    );
    return undefined;
  }
  const relPath = toWorkspaceRelativePath(folder, uri.fsPath);
  if (!relPath) {
    vscode.window.showErrorMessage("Remark: file is outside the workspace.");
    return undefined;
  }

  const meta = commentThreadMeta.get(thread);
  const isFileComment = meta?.isFileComment ?? false;
  const lineNumber = meta?.line ?? (thread.range ? thread.range.start.line + 1 : 1);
  const side = meta?.side ?? "new";

  if (isFileComment) {
    return {
      file: relPath,
      file_comment: true,
    };
  }

  return {
    file: relPath,
    line: lineNumber,
    side,
    file_comment: false,
  };
}

async function executeLspCommand(
  uri: vscode.Uri,
  command: string,
  args: { file: string; line?: number; side?: string; file_comment: boolean }
): Promise<void> {
  const folder = vscode.workspace.getWorkspaceFolder(uri);
  if (!folder) {
    vscode.window.showErrorMessage(
      "Remark: LSP is unavailable for this workspace."
    );
    return;
  }
  const client = clients.get(folder.uri.toString());
  if (!client) {
    vscode.window.showErrorMessage("Remark: LSP client is not running.");
    return;
  }
  try {
    await client.sendRequest("workspace/executeCommand", {
      command,
      arguments: [args],
    });
  } catch (err) {
    const msg = err instanceof Error ? err.message : String(err);
    vscode.window.showErrorMessage(`Remark: ${msg}`);
  }
}

async function startCommentThread(
  target: TargetDocument,
  lineNumber: number,
  isFileComment: boolean
): Promise<void> {
  if (!commentController) {
    vscode.window.showErrorMessage("Remark: comment controller not available.");
    return;
  }

  const lineIndex = Math.max(0, lineNumber - 1);
  if (isFileComment && hasExistingFileComment(target.uri)) {
    vscode.window.showErrorMessage("Remark: file comment already exists.");
    return;
  }
  if (!isFileComment && hasExistingLineComment(target.uri, lineIndex)) {
    vscode.window.showErrorMessage("Remark: line comment already exists.");
    return;
  }
  if (hasPendingThread(target.uri, lineIndex, isFileComment)) {
    return;
  }

  const document = await vscode.workspace.openTextDocument(target.uri);
  const range = makeThreadRange(lineIndex);
  await vscode.window.showTextDocument(document, {
    selection: range,
    preview: false,
  });

  const thread = commentController.createCommentThread(target.uri, range, []);
  thread.canReply = true;
  thread.collapsibleState = vscode.CommentThreadCollapsibleState.Expanded;
  thread.contextValue = isFileComment ? "remarkFile" : "remarkLine";
  commentThreadMeta.set(thread, {
    isFileComment,
    line: lineNumber,
    side: "new",
  });
  await focusCommentInput(thread);
}

function hasExistingLineComment(uri: vscode.Uri, lineIndex: number): boolean {
  const diagnostics = vscode.languages.getDiagnostics(uri);
  return diagnostics.some((diagnostic) => {
    if (!isRemarkDiagnostic(diagnostic)) {
      return false;
    }
    if (diagnostic.range.start.line !== lineIndex) {
      return false;
    }
    return /\(line\s+\d+\)$/.test(diagnostic.message.trim());
  });
}

function hasExistingFileComment(uri: vscode.Uri): boolean {
  const diagnostics = vscode.languages.getDiagnostics(uri);
  return diagnostics.some((diagnostic) => {
    if (!isRemarkDiagnostic(diagnostic)) {
      return false;
    }
    return !/\(line\s+\d+\)$/.test(diagnostic.message.trim());
  });
}

function hasPendingThread(
  uri: vscode.Uri,
  lineIndex: number,
  isFileComment: boolean
): boolean {
  for (const [thread, meta] of commentThreadMeta) {
    if (meta.key) {
      continue;
    }
    if (meta.isFileComment !== isFileComment) {
      continue;
    }
    if (thread.uri.toString() !== uri.toString()) {
      continue;
    }
    if (thread.range?.start.line !== lineIndex) {
      continue;
    }
    if (thread.comments.length > 0) {
      continue;
    }
    return true;
  }
  return false;
}

async function focusCommentInput(thread: vscode.CommentThread): Promise<void> {
  try {
    await vscode.commands.executeCommand("editor.action.commentThread.reply", thread);
    return;
  } catch {
    // Ignore and try alternate command.
  }
  try {
    await vscode.commands.executeCommand("commentThread.reply", thread);
  } catch {
    // Best-effort; user can click the reply box manually.
  }
}

async function addLineCommentCommand(
  uri?: vscode.Uri,
  line?: number
): Promise<void> {
  const target = resolveTargetDocument(uri);
  if (!target) {
    return;
  }

  const lineNumber = line ?? target.line;
  if (!lineNumber) {
    vscode.window.showErrorMessage("Remark: could not determine the line number.");
    return;
  }

  await startCommentThread(target, lineNumber, false);
}

async function addFileCommentCommand(uri?: vscode.Uri): Promise<void> {
  const target = resolveTargetDocument(uri);
  if (!target) {
    return;
  }

  await startCommentThread(target, 1, true);
}

type AddCommentArgs = {
  file: string;
  line?: number;
  side?: "old" | "new";
  fileComment: boolean;
  message: string;
};

function runRemarkAdd(
  folder: vscode.WorkspaceFolder,
  args: AddCommentArgs
): Promise<void> {
  const command = resolveRemarkCommand();
  const commandArgs = ["add", "--file", args.file, "--message", args.message];
  if (args.fileComment) {
    commandArgs.push("--file-comment");
  } else {
    if (!args.line || !args.side) {
      return Promise.reject(new Error("missing line or side for line comment"));
    }
    commandArgs.push("--line", args.line.toString(), "--side", args.side);
  }
  return new Promise((resolve, reject) => {
    cp.execFile(
      command,
      commandArgs,
      { cwd: folder.uri.fsPath, env: process.env },
      (error, stdout, stderr) => {
        if (error) {
          const detail = stderr?.toString().trim();
          reject(
            new Error(
              detail.length > 0
                ? detail
                : error.message || `failed to run ${command} add`
            )
          );
          return;
        }
        resolve();
      }
    );
  });
}

function resolveDraftPath(folder: vscode.WorkspaceFolder): Promise<string> {
  const command = "git";
  const args = ["rev-parse", "--git-dir"];
  return new Promise((resolve, reject) => {
    cp.execFile(
      command,
      args,
      { cwd: folder.uri.fsPath, env: process.env },
      (error, stdout, stderr) => {
        if (error) {
          const detail = stderr?.toString().trim();
          reject(
            new Error(
              detail.length > 0
                ? detail
                : error.message || `failed to run ${command} rev-parse --git-dir`
            )
          );
          return;
        }
        const gitDirRaw = stdout.toString().trim();
        if (!gitDirRaw) {
          reject(new Error("git rev-parse --git-dir returned an empty path"));
          return;
        }
        const gitDir = gitDirRaw.split(/\r?\n/)[0];
        const resolvedDir = path.isAbsolute(gitDir)
          ? gitDir
          : path.resolve(folder.uri.fsPath, gitDir);
        resolve(path.join(resolvedDir, "remark", "draft.md"));
      }
    );
  });
}

class RemarkTaskProvider implements vscode.TaskProvider {
  provideTasks(): vscode.Task[] {
    const folders = vscode.workspace.workspaceFolders ?? [];
    return folders.map((folder) => createOpenDraftTask(folder));
  }

  resolveTask(task: vscode.Task): vscode.Task | undefined {
    const definition = task.definition as { task?: string };
    if (definition.task !== TASK_OPEN_DRAFT) {
      return undefined;
    }

    const scope = task.scope;
    const folder =
      scope && typeof scope === "object" && "uri" in scope
        ? (scope as vscode.WorkspaceFolder)
        : vscode.workspace.workspaceFolders?.[0];

    if (!folder) {
      return undefined;
    }

    return createOpenDraftTask(folder);
  }
}

function createOpenDraftTask(folder: vscode.WorkspaceFolder): vscode.Task {
  const definition = { type: TASK_TYPE, task: TASK_OPEN_DRAFT };
  const execution = new vscode.CustomExecution(
    async () => new RemarkOpenDraftTerminal(folder)
  );
  const task = new vscode.Task(
    definition,
    folder,
    "remark: open draft",
    TASK_TYPE,
    execution
  );
  task.presentationOptions = {
    reveal: vscode.TaskRevealKind.Never,
    panel: vscode.TaskPanelKind.Dedicated,
  };
  return task;
}

class RemarkOpenDraftTerminal implements vscode.Pseudoterminal {
  private writeEmitter = new vscode.EventEmitter<string>();
  private closeEmitter = new vscode.EventEmitter<void>();

  onDidWrite = this.writeEmitter.event;
  onDidClose = this.closeEmitter.event;

  constructor(private folder: vscode.WorkspaceFolder) {}

  open(): void {
    void this.run();
  }

  close(): void {
    // No-op.
  }

  private async run(): Promise<void> {
    this.writeEmitter.fire("remark: opening draft...\r\n");
    try {
      const draftPath = await resolveDraftPath(this.folder);
      this.writeEmitter.fire(`remark: ${draftPath}\r\n`);
      await openDraftPath(draftPath);
      this.writeEmitter.fire("remark: done\r\n");
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      this.writeEmitter.fire(`remark: ${message}\r\n`);
      vscode.window.showErrorMessage(`Remark: ${message}`);
    } finally {
      this.closeEmitter.fire();
    }
  }
}

async function pickWorkspaceFolder(): Promise<vscode.WorkspaceFolder | undefined> {
  const active = vscode.window.activeTextEditor;
  if (active) {
    const folder = vscode.workspace.getWorkspaceFolder(active.document.uri);
    if (folder) {
      return folder;
    }
  }

  const folders = vscode.workspace.workspaceFolders ?? [];
  if (folders.length === 1) {
    return folders[0];
  }
  if (folders.length === 0) {
    return undefined;
  }

  return vscode.window.showWorkspaceFolderPick({
    placeHolder: "Select a workspace for remark",
  });
}

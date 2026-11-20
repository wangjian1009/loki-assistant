export interface WorkspaceHandle { readonly __workspace: string; }
export const open: (directory: string) => Promise<WorkspaceHandle>;
export const execute: (workspace: WorkspaceHandle, operation: string, payload: string) => Promise<string>;

export const cancel: (workspace: WorkspaceHandle, operationId: string) => void
export type HttpResponder = (status: number, body: string) => void
export type HttpHandler = (method: string, body: string, respond: HttpResponder) => void
export const startHttpServer: (workspace: WorkspaceHandle, handler: HttpHandler) => Promise<string>

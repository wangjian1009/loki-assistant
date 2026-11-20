export interface WorkspaceHandle { readonly __workspace: string; }
export const open: (directory: string) => Promise<WorkspaceHandle>;
export const execute: (workspace: WorkspaceHandle, operation: string, payload: string) => Promise<string>;

import { apiDelete, apiGet, apiPost, apiPut } from './client'
import type {
  BacklinksResponse,
  DailyNote,
  DocumentDto,
  DocumentMutation,
  NoteSummary,
  SearchResponse,
  TagDocument,
  TagSummary,
  TodoItem,
  WorkspaceOverview,
} from '@/types/workspace'

export const workspaceApi = {
  overview: () => apiGet<WorkspaceOverview>('/api/workspace'),
  notes: () => apiGet<NoteSummary[]>('/api/notes'),
  daily: () => apiGet<DailyNote[]>('/api/daily'),
  todos: () => apiGet<TodoItem[]>('/api/todos'),
  toggleTodo: (index: number) => apiPost<TodoItem[]>(`/api/todos/${index}/toggle`),
  search: (query: string) => apiGet<SearchResponse>(`/api/search?q=${encodeURIComponent(query)}`),
  tags: () => apiGet<TagSummary[]>('/api/tags'),
  tagDocuments: (tag: string) =>
    apiGet<TagDocument[]>(`/api/tags/${encodeURIComponent(tag)}`),
  document: (path: string) =>
    apiGet<DocumentDto>(`/api/document?path=${encodeURIComponent(path)}`),
  writeDocument: (path: string, source: string) =>
    apiPut<null>('/api/document', { path, source }),
  appendToday: (body: string) => apiPost<DailyNote>('/api/daily/today', { body }),
  backlinks: (path: string) =>
    apiGet<BacklinksResponse>(`/api/backlinks?path=${encodeURIComponent(path)}`),

  // --- Document lifecycle (the web counterpart of the TUI file actions). ---

  /** `POST /api/notes` — the backend owns extension/directory details. */
  createNote: (name: string, fromTemplate: boolean) =>
    apiPost<DocumentMutation>('/api/notes', { name, from_template: fromTemplate }),
  /** `POST /api/document/rename` — `newName` is a bare file name. */
  renameDocument: (path: string, newName: string) =>
    apiPost<DocumentMutation>('/api/document/rename', { path, new_name: newName }),
  /** `POST /api/document/archive` — moves a `data/` note into `archives/`. */
  archiveDocument: (path: string) =>
    apiPost<DocumentMutation>('/api/document/archive', { path }),
  /** `POST /api/document/restore` — moves an `archives/` note back to `data/`. */
  restoreDocument: (path: string) =>
    apiPost<DocumentMutation>('/api/document/restore', { path }),
  /** `DELETE /api/document?path=` — answers 204. */
  deleteDocument: (path: string) =>
    apiDelete<null>(`/api/document?path=${encodeURIComponent(path)}`),
  /** `DELETE /api/daily/{date}` — answers 204. */
  deleteDaily: (date: string) => apiDelete<null>(`/api/daily/${date}`),
  /** `POST /api/daily/{date}/move` — files the day into a `data/` note. */
  moveDaily: (date: string, targetPath: string) =>
    apiPost<DocumentMutation>(`/api/daily/${date}/move`, { target_path: targetPath }),
}

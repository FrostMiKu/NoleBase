import { apiGet, apiPut } from './client'
import type {
  BacklinksResponse,
  DailyNote,
  DocumentDto,
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
  search: (query: string) => apiGet<SearchResponse>(`/api/search?q=${encodeURIComponent(query)}`),
  tags: () => apiGet<TagSummary[]>('/api/tags'),
  tagDocuments: (tag: string) =>
    apiGet<TagDocument[]>(`/api/tags/${encodeURIComponent(tag)}`),
  document: (path: string) =>
    apiGet<DocumentDto>(`/api/document?path=${encodeURIComponent(path)}`),
  writeDocument: (path: string, source: string) =>
    apiPut<null>('/api/document', { path, source }),
  backlinks: (path: string) =>
    apiGet<BacklinksResponse>(`/api/backlinks?path=${encodeURIComponent(path)}`),
}

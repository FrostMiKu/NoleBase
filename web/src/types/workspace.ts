/**
 * DTOs served by the Axum web backend. Every shape mirrors
 * `src/web/dto.rs` (and the inline `json!` responses in `src/web/*.rs`)
 * exactly; paths on the wire are workspace-relative `/`-separated strings.
 */

export interface Health {
  ok: boolean
  version: string
}

/** `GET /api/workspace` */
export interface WorkspaceOverview {
  notes: number
  archives: number
  daily: number
  todos: number
  index_ready: boolean
}

/** One entry of `GET /api/notes` */
export interface NoteSummary {
  path: string
  name: string
  archived: boolean
  modified: string | null
}

/** One entry of `GET /api/daily` */
export interface DailyNote {
  date: string
  body: string
  path: string
}

/** One entry of `GET /api/todos` */
export interface TodoItem {
  checked: boolean
  text: string
}

/** One content-search match in `GET /api/search` */
export interface SearchHit {
  path: string | null
  line_no: number
  text: string
}

export interface SearchResponse {
  query: string
  hits: SearchHit[]
}

/** One entry of `GET /api/tags` */
export interface TagSummary {
  name: string
  documents: number
  mentions: number
}

/** One entry of `GET /api/tags/{tag}` */
export interface TagDocument {
  path: string
  modified: string | null
}

/** `GET /api/document` */
export interface DocumentDto {
  path: string
  source: string
}

/** `GET /api/backlinks?path=` */
export interface BacklinksResponse {
  path: string
  backlinks: string[]
}

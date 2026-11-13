import { apiDelete, apiGet, apiPost, apiPut } from './client'
import type { AgentSessionSummary } from '@/types/agent'
import type {
  AgentHistory,
  AgentJobRow,
  AgentStatus,
  PermissionMode,
  PermissionState,
} from '@/types/agent'

export type ApprovalDecision = 'approve' | 'deny'

function base(id: string | null): string {
  if (id === null) throw new Error('Choose an Agent session')
  return `/api/agents/${encodeURIComponent(id)}`
}

export const agentApi = {
  list: () => apiGet<{ sessions: AgentSessionSummary[] }>('/api/agents'),
  create: (title: string) => apiPost<AgentSessionSummary>('/api/agents', { title }),
  rename: (id: string, title: string) => apiPut<null>(base(id), { title }),
  delete: (id: string) => apiDelete<null>(base(id)),
  read: (id: string) => apiPost<null>(`${base(id)}/read`),
  status: (id: string | null) => apiGet<AgentStatus>(base(id)),
  history: (id: string | null) => apiGet<AgentHistory>(`${base(id)}/history`),
  jobs: (id: string | null) => apiGet<{ jobs: AgentJobRow[] }>(`${base(id)}/jobs`),
  run: (id: string | null, prompt: string) => apiPost<null>(`${base(id)}/run`, { prompt }),
  cancel: (id: string | null) => apiPost<null>(`${base(id)}/cancel`),
  approval: (id: string | null, decision: ApprovalDecision) => apiPost<null>(`${base(id)}/approval`, { decision }),
  answer: (id: string | null, answer: string) => apiPost<null>(`${base(id)}/answer`, { answer }),
  cancelAnswer: (id: string | null) => apiPost<null>(`${base(id)}/answer`, { cancel: true }),
  terminalInput: (id: string | null, text: string) => apiPost<null>(`${base(id)}/terminal-input`, { text }),
  cancelTerminalInput: (id: string | null) => apiPost<null>(`${base(id)}/terminal-input`, { cancel: true }),
  permission: (id: string | null) => apiGet<PermissionState>(`${base(id)}/permission`),
  setPermission: (id: string | null, mode: PermissionMode) => apiPut<PermissionState>(`${base(id)}/permission`, { mode }),
}

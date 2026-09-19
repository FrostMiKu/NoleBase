import { apiGet, apiPost, apiPut } from './client'
import type {
  AgentHistory,
  AgentJobRow,
  AgentStatus,
  PermissionMode,
  PermissionState,
} from '@/types/agent'

export type ApprovalDecision = 'approve' | 'deny'

export const agentApi = {
  status: () => apiGet<AgentStatus>('/api/agent'),
  history: () => apiGet<AgentHistory>('/api/agent/history'),
  jobs: () => apiGet<{ jobs: AgentJobRow[] }>('/api/agent/jobs'),
  run: (prompt: string) => apiPost<null>('/api/agent/run', { prompt }),
  cancel: () => apiPost<null>('/api/agent/cancel'),
  approval: (decision: ApprovalDecision) => apiPost<null>('/api/agent/approval', { decision }),
  answer: (answer: string) => apiPost<null>('/api/agent/answer', { answer }),
  cancelAnswer: () => apiPost<null>('/api/agent/answer', { cancel: true }),
  terminalInput: (text: string) => apiPost<null>('/api/agent/terminal-input', { text }),
  cancelTerminalInput: () => apiPost<null>('/api/agent/terminal-input', { cancel: true }),
  permission: () => apiGet<PermissionState>('/api/agent/permission'),
  setPermission: (mode: PermissionMode) => apiPut<PermissionState>('/api/agent/permission', { mode }),
}

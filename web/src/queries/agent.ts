import { useAgentStore } from '@/stores/agent-store'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { agentApi, type ApprovalDecision } from '@/api/agent'
import type { PermissionMode } from '@/types/agent'
import { toast } from 'sonner'
import { agentKeys } from './keys'

export function useAgentSessions() {
  return useQuery({ queryKey: agentKeys.sessions(), queryFn: agentApi.list, refetchInterval: 1000 })
}

export function useAgentStatus() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useQuery({ queryKey: agentKeys.state(id), queryFn: () => agentApi.status(id), enabled: id !== null })
}

export function useAgentPermission() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useQuery({ queryKey: agentKeys.permission(id), queryFn: () => agentApi.permission(id), enabled: id !== null })
}

export function useSetPermission() {
  const id = useAgentStore((state) => state.activeSessionId)
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (mode: PermissionMode) => agentApi.setPermission(id, mode),
    onMutate: async (mode) => {
      await queryClient.cancelQueries({ queryKey: agentKeys.permission(id) })
      const previous = queryClient.getQueryData(agentKeys.permission(id))
      queryClient.setQueryData(agentKeys.permission(id), { mode })
      return { previous, id }
    },
    onError: (_error, _mode, context) => {
      if (context?.previous !== undefined) {
        queryClient.setQueryData(agentKeys.permission(context.id), context.previous)
      }
      toast.error('Could not change permission mode')
    },
    onSettled: (_data, _error, _mode, context) => {
      void queryClient.invalidateQueries({ queryKey: agentKeys.permission(context?.id ?? id) })
    },
  })
}

export function useAgentRun() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useMutation({ mutationFn: (prompt: string) => agentApi.run(id, prompt) })
}

export function useAgentCancel() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useMutation({ mutationFn: () => agentApi.cancel(id) })
}

export function useAgentApproval() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useMutation({ mutationFn: (decision: ApprovalDecision) => agentApi.approval(id, decision) })
}

export function useAgentAnswer() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useMutation({ mutationFn: (answer: string) => agentApi.answer(id, answer) })
}

export function useAgentCancelAnswer() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useMutation({ mutationFn: () => agentApi.cancelAnswer(id) })
}

export function useAgentTerminalInput() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useMutation({ mutationFn: (text: string) => agentApi.terminalInput(id, text) })
}

export function useAgentCancelTerminalInput() {
  const id = useAgentStore((state) => state.activeSessionId)
  return useMutation({ mutationFn: () => agentApi.cancelTerminalInput(id) })
}

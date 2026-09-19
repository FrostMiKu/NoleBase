import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'

import { agentApi, type ApprovalDecision } from '@/api/agent'
import type { PermissionMode } from '@/types/agent'
import { toast } from 'sonner'
import { agentKeys } from './keys'

export function useAgentStatus() {
  return useQuery({ queryKey: agentKeys.state(), queryFn: agentApi.status })
}

export function useAgentPermission() {
  return useQuery({ queryKey: agentKeys.permission(), queryFn: agentApi.permission })
}

export function useSetPermission() {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: (mode: PermissionMode) => agentApi.setPermission(mode),
    onMutate: async (mode) => {
      await queryClient.cancelQueries({ queryKey: agentKeys.permission() })
      const previous = queryClient.getQueryData(agentKeys.permission())
      queryClient.setQueryData(agentKeys.permission(), { mode })
      return { previous }
    },
    onError: (_error, _mode, context) => {
      if (context?.previous !== undefined) {
        queryClient.setQueryData(agentKeys.permission(), context.previous)
      }
      toast.error('Could not change permission mode')
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: agentKeys.permission() })
    },
  })
}

export function useAgentRun() {
  return useMutation({ mutationFn: (prompt: string) => agentApi.run(prompt) })
}

export function useAgentCancel() {
  return useMutation({ mutationFn: () => agentApi.cancel() })
}

export function useAgentApproval() {
  return useMutation({ mutationFn: (decision: ApprovalDecision) => agentApi.approval(decision) })
}

export function useAgentAnswer() {
  return useMutation({ mutationFn: (answer: string) => agentApi.answer(answer) })
}

export function useAgentCancelAnswer() {
  return useMutation({ mutationFn: () => agentApi.cancelAnswer() })
}

export function useAgentTerminalInput() {
  return useMutation({ mutationFn: (text: string) => agentApi.terminalInput(text) })
}

export function useAgentCancelTerminalInput() {
  return useMutation({ mutationFn: () => agentApi.cancelTerminalInput() })
}

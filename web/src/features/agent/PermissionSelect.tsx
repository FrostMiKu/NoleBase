import { useAgentPermission, useSetPermission } from '@/queries/agent'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import type { PermissionMode } from '@/types/agent'

const MODES: { value: PermissionMode; label: string }[] = [
  { value: 'approve', label: 'Approve each action' },
  { value: 'auto', label: 'Auto — tools allowed' },
  { value: 'yolo', label: 'Yolo — no approvals' },
]

/** The backend's permission enum is the only source of options. */
export function PermissionSelect() {
  const permissionQuery = useAgentPermission()
  const setPermission = useSetPermission()
  const mode = permissionQuery.data?.mode

  return (
    <Select
      value={mode}
      onValueChange={(value) => {
        if (value === 'approve' || value === 'auto' || value === 'yolo') {
          setPermission.mutate(value)
        }
      }}
      disabled={permissionQuery.isLoading || mode === undefined}
    >
      <SelectTrigger aria-label="Permission mode" className="w-[130px] justify-between">
        <SelectValue placeholder="permission…" />
      </SelectTrigger>
      <SelectContent>
        {MODES.map((entry) => (
          <SelectItem key={entry.value} value={entry.value}>
            {entry.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  )
}

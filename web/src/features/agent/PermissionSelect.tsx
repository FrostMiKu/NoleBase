import { useAgentPermission, useSetPermission } from '@/queries/agent'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import type { PermissionMode } from '@/types/agent'

const MODES: { value: PermissionMode; short: string; label: string }[] = [
  { value: 'approve', short: 'Approve', label: 'Approve each action' },
  { value: 'auto', short: 'Auto', label: 'Auto — tools allowed' },
  { value: 'yolo', short: 'Yolo', label: 'Yolo — no approvals' },
]

/** The backend's permission enum is the only source of options. */
export function PermissionSelect() {
  const permissionQuery = useAgentPermission()
  const setPermission = useSetPermission()
  const mode = permissionQuery.data?.mode
  const current = MODES.find((entry) => entry.value === mode)

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
      <SelectTrigger
        aria-label="Permission mode"
        title={current?.label}
        className="w-[86px] justify-center gap-0.5 whitespace-nowrap"
      >
        {/* Short label in the trigger; the dropdown carries the full text. */}
        <SelectValue placeholder="permission…">{current?.short ?? mode}</SelectValue>
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

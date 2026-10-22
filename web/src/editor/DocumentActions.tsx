import { useState } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import {
  Archive,
  ArchiveRestore,
  Download,
  FileCode,
  LoaderCircle,
  MoreHorizontal,
  Pencil,
  Trash2,
} from 'lucide-react'
import { toast } from 'sonner'
import { workspaceApi } from '@/api/workspace'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { RenameNoteDialog } from '@/features/documents/RenameNoteDialog'
import { DeleteNoteDialog } from '@/features/documents/DeleteNoteDialog'
import { DeleteDailyDialog } from '@/features/navigation/DeleteDailyDialog'
import { useArchiveDocument, useRestoreDocument } from '@/queries/lifecycle'
import { workspaceKeys } from '@/queries/keys'
import { editorStore } from './editor-state'
import { currentEditorSource } from './editor-bridge'

const itemStyle =
  'flex w-full items-center gap-2 rounded px-2 py-2 text-left text-xs hover:bg-raised focus-visible:bg-raised disabled:opacity-50'

function downloadFile(content: string, name: string, type: string) {
  const url = URL.createObjectURL(new Blob([content], { type }))
  const link = document.createElement('a')
  link.href = url
  link.download = name
  link.click()
  window.setTimeout(() => URL.revokeObjectURL(url), 1000)
}

export function DocumentActions({ path, source }: { path: string; source: string }) {
  const [open, setOpen] = useState(false)
  const [action, setAction] = useState<'rename' | 'delete' | null>(null)
  const [exporting, setExporting] = useState(false)
  const queryClient = useQueryClient()
  const archive = useArchiveDocument()
  const restore = useRestoreDocument()
  const note = {
    path,
    name: path.split('/').pop() ?? path,
    archived: path.startsWith('archives/'),
    modified: null,
  }
  const daily = path.startsWith('daily/')
  const liveSource = () =>
    editorStore.getState().path === path ? (currentEditorSource() ?? source) : source
  const downloadHtml = async () => {
    if (exporting) return
    setExporting(true)
    try {
      const snapshot = liveSource()
      const preview = await queryClient.fetchQuery({
        queryKey: workspaceKeys.preview(path, snapshot),
        queryFn: () => workspaceApi.previewDocument(path, snapshot),
        staleTime: Infinity,
        gcTime: 0,
      })
      downloadFile(
        preview.html,
        note.name.replace(/\.(md|mb)$/i, '.html'),
        'text/html;charset=utf-8',
      )
      if (preview.warnings.length > 0)
        toast.warning(`HTML downloaded with ${preview.warnings.length} image warning(s)`)
      setOpen(false)
    } catch (error) {
      toast.error(`HTML export failed: ${error instanceof Error ? error.message : 'Unknown error'}`)
    } finally {
      setExporting(false)
    }
  }
  return (
    <>
      <Popover open={open} onOpenChange={setOpen}>
        <PopoverTrigger
          className="toolbar-button"
          title="More document actions"
          aria-label="More document actions"
        >
          {exporting ? (
            <LoaderCircle size={16} className="animate-spin" />
          ) : (
            <MoreHorizontal size={16} />
          )}
        </PopoverTrigger>
        <PopoverContent align="end" compact aria-label="Document actions">
          <button
            type="button"
            className={itemStyle}
            onClick={() => {
              downloadFile(liveSource(), note.name, 'text/markdown;charset=utf-8')
              setOpen(false)
            }}
          >
            <Download size={14} /> Download original
          </button>
          <button
            type="button"
            className={itemStyle}
            disabled={exporting}
            onClick={() => void downloadHtml()}
          >
            {exporting ? (
              <LoaderCircle size={14} className="animate-spin" />
            ) : (
              <FileCode size={14} />
            )}
            {exporting ? 'Exporting HTML…' : 'Download HTML'}
          </button>
          {!daily && (
            <>
              <div className="my-1 border-t border-border" />
              <button
                type="button"
                className={itemStyle}
                onClick={() => {
                  setOpen(false)
                  setAction('rename')
                }}
              >
                <Pencil size={14} /> Rename note
              </button>
              <button
                type="button"
                className={itemStyle}
                disabled={archive.isPending || restore.isPending}
                onClick={() => {
                  setOpen(false)
                  note.archived ? restore.mutate(path) : archive.mutate(path)
                }}
              >
                {note.archived ? <ArchiveRestore size={14} /> : <Archive size={14} />}
                {note.archived ? 'Restore note' : 'Archive note'}
              </button>
            </>
          )}
          <div className="my-1 border-t border-border" />
          <button
            type="button"
            className={`${itemStyle} text-danger`}
            onClick={() => {
              setOpen(false)
              setAction('delete')
            }}
          >
            <Trash2 size={14} /> Delete document
          </button>
        </PopoverContent>
      </Popover>
      {action === 'rename' && <RenameNoteDialog note={note} onClose={() => setAction(null)} />}
      {action === 'delete' &&
        (daily ? (
          <DeleteDailyDialog
            note={{ path, date: note.name.replace(/\.md$/, ''), body: liveSource() }}
            onClose={() => setAction(null)}
          />
        ) : (
          <DeleteNoteDialog note={note} onClose={() => setAction(null)} />
        ))}
    </>
  )
}

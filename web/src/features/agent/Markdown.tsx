import ReactMarkdown from 'react-markdown'
import { remarkNoteLinks } from '@/features/documents/note-links'
import remarkGfm from 'remark-gfm'
import type { ComponentProps } from 'react'

type Components = ComponentProps<typeof ReactMarkdown>['components']

/**
 * Dense dark Markdown rendering for agent output. Agent text is plain
 * markdown (not MBDown documents), so a standard renderer with GFM covers
 * headings, lists, tables, code, and links. Styling is mapped per element
 * to the app's tokens; links open in a new tab.
 */
const components: Components = {
  h1: ({ children }) => <h1 className="mt-2 mb-1 text-[14px] font-semibold text-fg">{children}</h1>,
  h2: ({ children }) => <h2 className="mt-2 mb-1 text-[13px] font-semibold text-fg">{children}</h2>,
  h3: ({ children }) => (
    <h3 className="mt-1.5 mb-1 text-[12px] font-semibold text-fg">{children}</h3>
  ),
  h4: ({ children }) => (
    <h4 className="mt-1.5 mb-1 text-[12px] font-semibold text-fg">{children}</h4>
  ),
  p: ({ children }) => <p className="my-1 leading-[1.5]">{children}</p>,
  ul: ({ children }) => <ul className="my-1 list-disc pl-4">{children}</ul>,
  ol: ({ children }) => <ol className="my-1 list-decimal pl-4">{children}</ol>,
  li: ({ children }) => <li className="my-px leading-[1.5]">{children}</li>,
  blockquote: ({ children }) => (
    <blockquote className="my-1 border-l-2 border-border-strong pl-2 text-muted">
      {children}
    </blockquote>
  ),
  a: ({ children, href }) => (
    <a
      href={href}
      target="_blank"
      rel="noreferrer"
      className="text-accent underline decoration-accent/40 hover:decoration-accent"
    >
      {children}
    </a>
  ),
  table: ({ children }) => (
    <div className="my-1.5 overflow-x-auto">
      <table className="w-full border-collapse font-mono text-[11px]">{children}</table>
    </div>
  ),
  th: ({ children }) => (
    <th className="border border-border bg-panel px-1.5 py-0.5 text-left font-semibold text-fg">
      {children}
    </th>
  ),
  td: ({ children }) => (
    <td className="border border-border px-1.5 py-0.5 align-top">{children}</td>
  ),
  pre: ({ children }) => (
    <pre className="my-1.5 overflow-x-auto rounded-sm border border-border bg-bg px-2 py-1.5 font-mono text-[11px] leading-[1.5] text-fg">
      {children}
    </pre>
  ),
  code: ({ className, children }) => {
    const isBlock = typeof className === 'string' && className.startsWith('language-')
    if (isBlock) {
      return <code className="font-mono">{children}</code>
    }
    return (
      <code className="rounded-sm border border-border bg-panel px-1 py-px font-mono text-[11px] text-accent-strong">
        {children}
      </code>
    )
  },
  hr: () => <hr className="my-2 border-border" />,
}

export function Markdown({
  text,
  noteLinks = false,
  onFollowLink,
}: {
  text: string
  noteLinks?: boolean
  onFollowLink?: (href: string) => boolean
}) {
  const renderedComponents: Components = {
    ...components,
    a: ({ children, href }) => (
      <a
        href={href}
        target="_blank"
        rel="noreferrer"
        className="text-accent underline decoration-accent/40 hover:decoration-accent"
        onClick={(event) => {
          if (href && onFollowLink?.(href)) event.preventDefault()
        }}
      >
        {children}
      </a>
    ),
    img: ({ src, alt }) => (
      <img
        src={src}
        alt={alt ?? ''}
        loading="lazy"
        className="my-3 max-h-96 max-w-full rounded object-contain"
      />
    ),
  }
  return (
    <div className="min-w-0 text-[13px] break-words text-fg [overflow-wrap:anywhere]">
      <ReactMarkdown
        remarkPlugins={noteLinks ? [remarkGfm, remarkNoteLinks] : [remarkGfm]}
        components={renderedComponents}
      >
        {text}
      </ReactMarkdown>
    </div>
  )
}

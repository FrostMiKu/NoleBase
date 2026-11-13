import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it } from 'vitest'

import { Markdown } from './Markdown'
import { useUiStore } from '@/stores/ui-store'

afterEach(cleanup)

describe('Markdown', () => {
  const id = 'ca24169f-9431-4645-8eb3-0a88c1dae545'

  it('renders managed image URIs and provides downloads for attachment links', () => {
    render(
      <Markdown
        noteLinks
        text={`![潜水照片](nole://attachment/${id})\n\n[报告](nole://attachment/${id})`}
      />,
    )
    expect(screen.getByRole('img', { name: '潜水照片' })).toHaveAttribute(
      'src',
      `/api/attachments/${id}`,
    )
    expect(screen.getByRole('link', { name: '报告' })).toHaveAttribute(
      'href',
      `/api/attachments/${id}?download=true`,
    )
  })

  it('continues to reject unsafe and malformed attachment URLs', () => {
    render(
      <Markdown
        text={`[unsafe](javascript:alert%281%29) [bad](nole://attachment/../../config) [web](https://example.com) ![missing](nole://attachment/bad)`}
      />,
    )
    expect(screen.getByText('unsafe')).not.toHaveAttribute(
      'href',
      expect.stringContaining('javascript:'),
    )
    expect(screen.getByText('bad')).not.toHaveAttribute(
      'href',
      expect.stringContaining('/api/attachments/'),
    )
    expect(screen.getByRole('link', { name: 'web' })).toHaveAttribute('href', 'https://example.com')
    expect(screen.getByText('Image unavailable: missing')).toBeInTheDocument()
  })

  it('offers retry for missing images and reloads managed images after attachment updates', () => {
    render(<Markdown text={`![photo](nole://attachment/${id})`} />)
    const initial = screen.getByRole('img')
    fireEvent.error(initial)
    expect(screen.getByText('Image unavailable: photo')).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }))
    const retried = screen.getByRole('img')
    expect(retried).not.toBe(initial)
    act(() => useUiStore.getState().refreshAttachmentImages())
    expect(screen.getByRole('img')).not.toBe(retried)
  })

  it('renders headings, lists, code, and GFM tables', () => {
    render(
      <Markdown
        text={[
          '## 结论',
          '',
          '- 第一条 **重点**',
          '- 第二条',
          '',
          '```rust',
          'fn main() {}',
          '```',
          '',
          '| 列 A | 列 B |',
          '| --- | --- |',
          '| 1 | 2 |',
          '',
          '~~划掉~~ 和 [链接](https://example.com)',
        ].join('\n')}
      />,
    )
    expect(screen.getByRole('heading', { name: '结论' })).toBeTruthy()
    expect(screen.getAllByRole('listitem').length).toBe(2)
    expect(screen.getByRole('table')).toBeTruthy()
    expect(screen.getByText('fn main() {}')).toBeTruthy()
    expect(screen.getByText('链接')).toBeTruthy()
  })

  it('keeps non-markdown syntax (BBCode/wikilinks) as plain text', () => {
    const { container } = render(<Markdown text={'[box]frame[/box] links to [[note-a]]'} />)
    expect(container.textContent).toBe('[box]frame[/box] links to [[note-a]]')
  })
})

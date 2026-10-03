import { describe, expect, it } from 'bun:test'
import type { Message } from '../types'
import {
  THREAD_LIST_PAGE_SIZE,
  dropLocallyRemovedThreads,
  mergeRefreshedThreadPage,
  nextSelectedAfterRefresh,
  releaseRemovedThread,
  suppressRemovedThread,
} from './mail'

function row(id: string, date: number): Message {
  return {
    id,
    account_id: 'acc',
    folder_id: 'INBOX',
    thread_id: id,
    from_name: '',
    from_addr: '',
    to: '',
    subject: id,
    preview: '',
    body: '',
    date,
    unread: false,
    starred: false,
    has_attachments: false,
  }
}

describe('mergeRefreshedThreadPage', () => {
  it('drops a thread the refreshed page no longer has', () => {
    const previous = [row('archived', 30), row('kept', 20), row('older', 1)]
    const fetched = [row('kept', 20), row('slid-up', 10)]
    expect(mergeRefreshedThreadPage(previous, fetched).map((thread) => thread.thread_id)).toEqual(['kept', 'slid-up'])
  })

  it('keeps threads the reader scrolled to, past a full first page', () => {
    const fetched = Array.from({ length: THREAD_LIST_PAGE_SIZE }, (_, index) => row(`t${index}`, 200 - index))
    const previous = [row('archived', 180), ...fetched, row('scrolled', 10)]
    expect(mergeRefreshedThreadPage(previous, fetched).map((thread) => thread.thread_id)).toEqual([
      ...fetched.map((thread) => thread.thread_id),
      'scrolled',
    ])
  })

  it('puts newly arrived mail first and leaves older mail behind it', () => {
    const previous = [row('old', 10)]
    const fetched = [row('new', 20), row('old', 10)]
    expect(mergeRefreshedThreadPage(previous, fetched).map((thread) => thread.thread_id)).toEqual(['new', 'old'])
  })

  it('does not lift an older thread above newer mail already on screen', () => {
    const previous = [row('oct-1', 200), row('oct-2', 190)]
    const fetched = [row('sept', 100), row('oct-1', 200), row('oct-2', 190)]
    expect(mergeRefreshedThreadPage(previous, fetched).map((thread) => thread.thread_id)).toEqual([
      'oct-1',
      'oct-2',
      'sept',
    ])
  })

  it('keeps the list when the refresh comes back empty', () => {
    const previous = [row('kept', 10)]
    expect(mergeRefreshedThreadPage(previous, [])).toEqual(previous)
  })

  it('keeps scrolled threads when a short page still has more below', () => {
    const previous = [row('top', 30), row('open', 20), row('scrolled', 1)]
    const fetched = [row('top', 30)]
    expect(mergeRefreshedThreadPage(previous, fetched, true).map((thread) => thread.thread_id)).toEqual([
      'top',
      'open',
      'scrolled',
    ])
  })

  it('drops scrolled threads when a short page is the end of the folder', () => {
    const previous = [row('top', 30), row('open', 20), row('scrolled', 1)]
    const fetched = [row('top', 30)]
    expect(mergeRefreshedThreadPage(previous, fetched, false).map((thread) => thread.thread_id)).toEqual(['top'])
  })
})

describe('nextSelectedAfterRefresh', () => {
  it('follows the open conversation to the next row the refresh kept', () => {
    const previous = [row('archived', 30), row('open', 20), row('next', 10)]
    const visible = [row('next', 10)]
    expect(nextSelectedAfterRefresh(previous, visible, 'open')).toBe('next')
  })

  it('steps backward when the open thread was the last row kept', () => {
    const previous = [row('prev', 20), row('open', 10)]
    const visible = [row('prev', 20)]
    expect(nextSelectedAfterRefresh(previous, visible, 'open')).toBe('prev')
  })

  it('leaves a selection that was never in this list', () => {
    const visible = [row('next', 10)]
    expect(nextSelectedAfterRefresh([], visible, 'notification')).toBe('notification')
  })

  it('clears the selection when the refresh kept nothing', () => {
    const previous = [row('open', 10)]
    expect(nextSelectedAfterRefresh(previous, [], 'open')).toBe('')
  })
})

describe('dropLocallyRemovedThreads', () => {
  it('keeps an archived thread out of a background refresh that still has the row', () => {
    const archived = row('acc#INBOX#t.archived', 5)
    const neighbour = row('acc#INBOX#t.neighbour', 4)
    const startedBefore = 0
    const undo = suppressRemovedThread(archived.thread_id)
    expect(
      dropLocallyRemovedThreads([archived, neighbour], [archived], startedBefore, false).map((thread) => thread.thread_id),
    ).toEqual([neighbour.thread_id])
    expect(
      dropLocallyRemovedThreads([archived, neighbour], [archived], Number.MAX_SAFE_INTEGER, false).map(
        (thread) => thread.thread_id,
      ),
    ).toEqual([neighbour.thread_id])
    undo()
    expect(
      dropLocallyRemovedThreads([archived, neighbour], [], startedBefore, false).map((thread) => thread.thread_id),
    ).toEqual([archived.thread_id, neighbour.thread_id])
    suppressRemovedThread(archived.thread_id)
    expect(
      dropLocallyRemovedThreads([archived, neighbour], [archived], Number.MAX_SAFE_INTEGER, true).map(
        (thread) => thread.thread_id,
      ),
    ).toEqual([archived.thread_id, neighbour.thread_id])
    suppressRemovedThread(archived.thread_id)
    releaseRemovedThread(archived.thread_id)
    expect(
      dropLocallyRemovedThreads([archived, neighbour], [], startedBefore, false).map((thread) => thread.thread_id),
    ).toEqual([archived.thread_id, neighbour.thread_id])
  })
})

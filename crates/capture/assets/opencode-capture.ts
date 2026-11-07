import { spawn } from 'node:child_process'

const FINISHED = new Set(['session.execution.succeeded', 'session.execution.failed', 'session.execution.interrupted'])

const COMMAND = __COMMAND__

type SessionEvent = {
  type?: string
  location?: { directory?: string }
  data?: { sessionID?: string }
}

type Context = {
  location?: { directory?: string }
  event: { subscribe: (options: { signal: AbortSignal }) => AsyncIterable<SessionEvent> }
}

function capture(sessionID: string, directory: string | undefined): void {
  try {
    const payload = JSON.stringify({ session_id: sessionID, hook_event_name: 'session.idle', cwd: directory })
    const child = spawn('bash', ['-c', COMMAND, 'rh-capture', payload], { stdio: 'ignore', detached: true })
    child.on('error', () => {})
    child.unref()
  } catch {}
}

async function watch(ctx: Context, signal: AbortSignal): Promise<void> {
  try {
    for await (const event of ctx.event.subscribe({ signal })) {
      if (typeof event?.type !== 'string' || !FINISHED.has(event.type)) continue
      const id = event.data?.sessionID
      if (typeof id === 'string' && id.length > 0) capture(id, event.location?.directory ?? ctx.location?.directory)
    }
  } catch {}
}

export default {
  id: 'rivethub-capture',
  setup: (ctx: Context) => {
    const controller = new AbortController()
    void watch(ctx, controller.signal)
    return () => controller.abort()
  },
}

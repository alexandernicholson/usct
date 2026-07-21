import type { ExtensionAPI, ExtensionContext } from "@oh-my-pi/pi-coding-agent";
import { truncateToWidth } from "@oh-my-pi/pi-tui";

const USCT_BIN = process.env.USCT_BIN || "usct";
const ZERO_STATUS = "$0.00 · 0";

function contextPercent(ctx: ExtensionContext): number | undefined {
  const tokens = ctx.getContextUsage()?.tokens;
  const capacity = ctx.model?.contextWindow;
  if (tokens === undefined || capacity === undefined || capacity <= 0) return undefined;
  return Math.min(100, Math.max(0, (tokens / capacity) * 100));
}

function displayStatus(ctx: ExtensionContext, text: string): void {
  ctx.ui.setStatus("usct", undefined);
  if (!text) {
    ctx.ui.setWidget("usct", undefined);
    return;
  }

  ctx.ui.setWidget(
    "usct",
    (_tui, theme) => ({
      render(width) {
        const styled = text
          .split(" · ")
          .map((part) => {
            if (part.startsWith("usct:")) return theme.fg("error", part);
            if (part.startsWith("$")) return theme.fg("statusLineCost", part);
            if (part.startsWith("burn ")) {
              const level = part.slice(5);
              const color = level === "high" ? "error" : level === "medium" ? "warning" : "success";
              return theme.fg(color, part);
            }

            const context = Number.parseFloat(part);
            if (part.endsWith("% context") && Number.isFinite(context)) {
              const color = context >= 80 ? "error" : context >= 50 ? "warning" : "success";
              return theme.fg(color, part);
            }
            return theme.fg("statusLineSpend", part);
          })
          .join(theme.fg("dim", " · "));
        return [truncateToWidth(styled, width)];
      },
    }),
    { placement: "aboveEditor" },
  );
}

async function refreshStatus(ctx: ExtensionContext): Promise<void> {
  if (!ctx.hasUI) return;

  const transcriptPath = ctx.sessionManager.getSessionFile();
  if (!transcriptPath || !(await Bun.file(transcriptPath).exists())) {
    displayStatus(ctx, ZERO_STATUS);
    return;
  }

  const payload: Record<string, unknown> = { transcript_path: transcriptPath };
  const usedPercentage = contextPercent(ctx);
  if (usedPercentage !== undefined) {
    payload.context_window = { used_percentage: usedPercentage };
  }

  try {
    const child = Bun.spawn(
      [
        USCT_BIN,
        "omp",
        "statusline",
        "--cost-source",
        "both",
        "--visual-burn-rate",
        "text",
      ],
      { stdin: "pipe", stdout: "pipe", stderr: "pipe" },
    );
    child.stdin.write(JSON.stringify(payload));
    child.stdin.end();

    const [stdout, stderr, exitCode] = await Promise.all([
      new Response(child.stdout).text(),
      new Response(child.stderr).text(),
      child.exited,
    ]);
    const detail = stderr.trim().replace(/^usct:\s*/, "");
    let text = stdout.trim();
    if (exitCode !== 0) {
      text = detail === "session contains no token usage" ? ZERO_STATUS : `usct: ${detail || `exit ${exitCode}`}`;
    }
    displayStatus(ctx, text);
  } catch (error) {
    displayStatus(ctx, `usct: ${error instanceof Error ? error.message : String(error)}`);
  }
}

export default function usctStatusline(pi: ExtensionAPI): void {
  pi.on("session_start", async (_event, ctx) => refreshStatus(ctx));
  pi.on("session_switch", async (_event, ctx) => refreshStatus(ctx));
  pi.on("session_branch", async (_event, ctx) => refreshStatus(ctx));
  pi.on("session_tree", async (_event, ctx) => refreshStatus(ctx));
  pi.on("session_compact", async (_event, ctx) => refreshStatus(ctx));
  pi.on("turn_end", async (_event, ctx) => refreshStatus(ctx));
}

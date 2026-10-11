// Panel side of "权限与诊断": the red dot on the settings button and the "others cannot connect
// to this device" banner. Rust owns the state (dismissal survives the panel webview being
// destroyed and re-created; a new occurrence or the next start shows it again).
import { api, onDiag, type DiagAlert } from "./api";

class DiagUi {
  problems = $state(0);
  alert = $state<DiagAlert | null>(null);
  private started = false;

  async init() {
    if (this.started) return;
    this.started = true;
    await onDiag((s) => this.apply(s.problems, s.alert));
    try {
      const s = await api.diagStatus();
      this.apply(s.problems, s.alert);
    } catch {
      /* older backend: no diagnostics */
    }
  }

  private apply(problems: number, alert: DiagAlert | null) {
    this.problems = problems;
    this.alert = alert;
  }

  dismiss() {
    this.alert = null;
    api.dismissAlert().catch(() => {});
  }

  fix() {
    api.openDiagnostics().catch(() => {});
  }
}

export const diag = new DiagUi();

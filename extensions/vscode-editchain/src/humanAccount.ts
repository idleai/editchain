import * as vscode from 'vscode';

export const HUMAN_ACCOUNT_SCOPES = ['read:user', 'read:org'];

/** Display metadata only; device identity and trust remain independent. */
export class HumanAccount implements vscode.Disposable {
  name: string | undefined;
  private generation = 0;
  private disposed = false;
  private readonly subscription: vscode.Disposable;

  constructor(private readonly changed: (name: string | undefined) => void) {
    this.subscription = vscode.authentication.onDidChangeSessions(event => {
      if (event.provider.id === 'github') void this.refresh();
    });
    void this.refresh();
  }

  use(account: vscode.AuthenticationSessionAccountInformation | undefined): void {
    if (this.disposed) return;
    this.generation++;
    const label = account?.label.trim();
    const name = label && [...label].length <= 80 && !/\p{Cc}/u.test(label) ? label : undefined;
    if (name === this.name) return;
    this.name = name;
    this.changed(name);
  }

  async refresh(): Promise<void> {
    const generation = ++this.generation;
    if (!vscode.workspace.isTrusted) { this.use(undefined); return; }
    try {
      const session = await vscode.authentication.getSession('github', HUMAN_ACCOUNT_SCOPES, { silent: true });
      if (generation === this.generation) this.use(session?.account);
    } catch {
      // Missing or unavailable sign-in must not stop local capture.
      if (generation === this.generation) this.use(undefined);
    }
  }

  dispose(): void { this.disposed = true; this.generation++; this.subscription.dispose(); }
}

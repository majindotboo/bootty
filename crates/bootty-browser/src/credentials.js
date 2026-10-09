(() => {
  if (window !== window.top) return;
  const bytes = new Uint32Array(4);
  crypto.getRandomValues(bytes);
  const token = [...bytes].map(value => value.toString(16).padStart(8, '0')).join('');
  const visible = field => field.isConnected && !field.disabled && !field.readOnly && field.getClientRects().length > 0 && getComputedStyle(field).visibility !== 'hidden';
  const set = (field, value) => {
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')?.set;
    if (!setter) return false;
    setter.call(field, value);
    field.dispatchEvent(new Event('input', { bubbles: true }));
    field.dispatchEvent(new Event('change', { bubbles: true }));
    return true;
  };
  Object.defineProperty(window, '__boottyCredentials', {
    configurable: false, writable: false,
    value: Object.freeze({
      document() { return token; },
      fill(request) {
        const current = () => window === window.top && location.origin === request.origin && token === request.document;
        if (!current()) return false;
        const candidates = document.querySelectorAll('input[type="password"]');
        // Reject large or ambiguous forms; expand only with explicit user field selection.
        if (candidates.length > 32) return false;
        const passwords = [...candidates].filter(visible);
        if (passwords.length !== 1) return false;
        const password = passwords[0], form = password.form;
        if (password.autocomplete === 'new-password' || !form || form.elements.length > 128) return false;
        const accounts = [...form.elements].filter(field => field instanceof HTMLInputElement && visible(field) && ['text', 'email'].includes(field.type) && !['one-time-code', 'new-password'].includes(field.autocomplete));
        if (accounts.length !== 1 || !current()) return false;
        if (!set(accounts[0], request.account) || !visible(password) || !current()) return false;
        return set(password, request.password) && current();
      }
    })
  });
})();

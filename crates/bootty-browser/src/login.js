(origin, username, password) => {
    if (location.origin !== origin) return 'The page changed. Open saved logins again.';
    const visible = element => !element.disabled && !element.readOnly && element.getClientRects().length > 0;
    const passwords = [...document.querySelectorAll('input[type="password"]')]
        .filter(element => visible(element) && element.autocomplete !== 'new-password');
    if (passwords.length !== 1) return 'Choose a page with one visible sign-in form.';
    const passwordInput = passwords[0];
    const scope = passwordInput.form || document;
    const candidates = [...scope.querySelectorAll('input')].filter(element => visible(element)
        && ['text', 'email'].includes(element.type)
        && Boolean(element.compareDocumentPosition(passwordInput) & Node.DOCUMENT_POSITION_FOLLOWING));
    const named = candidates.filter(element => element.autocomplete === 'username');
    const usernames = named.length ? named : candidates;
    if (usernames.length !== 1) return 'Fill the username directly; this form is ambiguous.';
    const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value').set;
    for (const [element, value] of [[usernames[0], username], [passwordInput, password]]) {
        setter.call(element, value);
        element.dispatchEvent(new Event('input', { bubbles: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
    }
    return 'filled';
}

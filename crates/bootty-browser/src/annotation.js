(() => {
    let enabled = false;
    let highlighted = null;
    let oldOutline = '';
    let picked = null;
    let editor = null;
    let removeEditor = () => {};
    const clear = () => {
        if (highlighted) highlighted.style.outline = oldOutline;
        highlighted = null;
    };
    const close = () => {
        removeEditor();
        clear();
        picked = null;
    };
    window.__boottyAnnotate = value => {
        close();
        enabled = Boolean(value);
    };
    window.__boottyAnnotationEditor = theme => {
        if (!picked || !picked.isConnected) {
            window.ipc.postMessage('browser-annotation:' + JSON.stringify({action: 'cancel'}));
            return;
        }
        removeEditor();
        const target = picked;
        const previousFocus = document.activeElement;
        const host = document.createElement('div');
        host.style.cssText = 'all:initial!important;position:fixed!important;inset:0!important;z-index:2147483647!important;pointer-events:none!important;';
        const shadow = host.attachShadow({mode: 'closed'});
        const style = document.createElement('style');
        style.textContent = `
            :host { color-scheme: normal; }
            * { box-sizing: border-box; }
            .editor { position: absolute; pointer-events: auto; display: flex; flex-direction: column;
                gap: .5em; padding: .75em; background: var(--background); color: var(--foreground);
                border: 1px solid var(--border); border-radius: var(--radius);
                font: var(--font-size) -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
                overflow: auto; }
            header { display: flex; align-items: center; justify-content: space-between; gap: .5em; font-weight: 600; }
            textarea { display: block; width: 100%; min-height: 4.5em; resize: none; padding: .5em;
                border: 1px solid var(--border); border-radius: var(--radius); background: var(--background);
                color: var(--foreground); font: inherit; }
            textarea::placeholder { color: var(--muted); }
            footer { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: .5em; }
            button { font: inherit; padding: .35em .65em; border: 1px solid var(--border);
                border-radius: var(--radius); color: var(--foreground); background: var(--background); }
            button.primary { color: var(--primary-foreground); background: var(--primary); border-color: var(--primary); }
            button.close { border: 0; padding: .1em .4em; background: transparent; }
            button:disabled { opacity: .5; }
            button:focus-visible, textarea:focus-visible { outline: 2px solid var(--primary); outline-offset: 2px; }
        `;
        const panel = document.createElement('section');
        panel.className = 'editor';
        panel.setAttribute('role', 'dialog');
        panel.setAttribute('aria-label', 'Comment on selected element');
        for (const name of ['background', 'foreground', 'muted', 'border', 'primary', 'primary_foreground']) {
            panel.style.setProperty('--' + name.replaceAll('_', '-'), '#' + (theme[name] >>> 0).toString(16).padStart(8, '0'));
        }
        const unit = Math.max(1, theme.font_size);
        panel.style.setProperty('--font-size', unit + 'px');
        panel.style.setProperty('--radius', Math.max(0, theme.radius) + 'px');
        const header = document.createElement('header');
        header.append(document.createTextNode('Comment'));
        const button = (label, className, action) => {
            const result = document.createElement('button');
            result.type = 'button';
            result.textContent = label;
            result.className = className;
            result.addEventListener('click', event => {
                if (!event.isTrusted) return;
                event.preventDefault();
                action();
            });
            return result;
        };
        const send = payload => {
            const {action, ...value} = payload;
            window.ipc.postMessage('browser-annotation:' + JSON.stringify(action === 'cancel' ? {action} : {action, value}));
        };
        const dismiss = () => {
            close();
            send({action: 'cancel'});
        };
        const dismissButton = button('×', 'close', dismiss);
        dismissButton.setAttribute('aria-label', 'Close comment');
        header.append(dismissButton);
        const comment = document.createElement('textarea');
        comment.rows = 3;
        comment.maxLength = 4096;
        comment.placeholder = 'Describe the change you want';
        comment.setAttribute('aria-label', 'Requested change');
        const footer = document.createElement('footer');
        const copy = button('Copy', '', () => send({action: 'copy', comment: comment.value}));
        const paste = button('Paste into terminal', 'primary', () => send({action: 'paste', comment: comment.value}));
        paste.title = navigator.platform.includes('Mac') ? 'Paste into terminal (⌘Enter)' : 'Paste into terminal (Ctrl+Enter)';
        footer.append(copy, paste);
        panel.append(header, comment, footer);
        shadow.append(style, panel);
        document.documentElement.append(host);
        editor = host;
        highlighted = target;
        oldOutline = target.style.outline;
        target.style.outline = '2px solid Highlight';
        const position = () => {
            if (!target.isConnected) { dismiss(); return; }
            const margin = unit * .5;
            const rect = target.getBoundingClientRect();
            panel.style.width = Math.max(1, Math.min(unit * 23, innerWidth - margin * 2)) + 'px';
            panel.style.maxHeight = Math.max(1, innerHeight - margin * 2) + 'px';
            const width = panel.offsetWidth;
            const height = panel.offsetHeight;
            const clamp = (value, maximum) => Math.max(margin, Math.min(value, Math.max(margin, maximum)));
            panel.style.left = clamp(rect.left, innerWidth - width - margin) + 'px';
            const below = rect.bottom + margin;
            const above = rect.top - height - margin;
            panel.style.top = clamp(below + height <= innerHeight - margin ? below : above, innerHeight - height - margin) + 'px';
        };
        const keyboard = event => {
            if (!event.isTrusted) return;
            const command = (event.metaKey || event.ctrlKey) && !event.altKey;
            if (event.key !== 'Escape' && !(event.key === 'Enter' && command && !event.shiftKey && event.composedPath().includes(host))) return;
            event.preventDefault();
            event.stopImmediatePropagation();
            if (event.key === 'Escape') { dismiss(); return; }
            if (!paste.disabled) send({action: 'key', key: event.key, command, shift: event.shiftKey, comment: comment.value});
        };
        const update = () => {
            const invalid = !comment.value.trim() || new TextEncoder().encode(comment.value).length > 4096;
            copy.disabled = invalid;
            paste.disabled = invalid;
        };
        const outside = event => {
            if (event.isTrusted && !event.composedPath().includes(host)) dismiss();
        };
        comment.addEventListener('input', update);
        document.addEventListener('pointerdown', outside, true);
        document.addEventListener('keydown', keyboard, true);
        window.addEventListener('resize', position);
        document.addEventListener('scroll', position, true);
        removeEditor = () => {
            document.removeEventListener('pointerdown', outside, true);
            document.removeEventListener('keydown', keyboard, true);
            window.removeEventListener('resize', position);
            document.removeEventListener('scroll', position, true);
            host.remove();
            if (previousFocus instanceof HTMLElement && previousFocus.isConnected) previousFocus.focus({preventScroll: true});
            editor = null;
            removeEditor = () => {};
        };
        update();
        position();
        comment.focus({preventScroll: true});
    };
    document.addEventListener('keydown', event => {
        if (enabled && event.isTrusted && event.key === 'Escape') {
            event.preventDefault();
            event.stopImmediatePropagation();
            enabled = false;
            close();
            window.ipc.postMessage('browser-annotation:' + JSON.stringify({action: 'cancel'}));
        }
    }, true);
    document.addEventListener('pointermove', event => {
        if (!enabled || !event.isTrusted || !(event.target instanceof Element) || editor) return;
        if (highlighted === event.target) return;
        clear();
        highlighted = event.target;
        oldOutline = highlighted.style.outline;
        highlighted.style.outline = '2px solid Highlight';
    }, true);
    document.addEventListener('click', event => {
        if (!enabled || !event.isTrusted || !(event.target instanceof Element) || editor) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        const target = event.target;
        clear();
        enabled = false;
        picked = target;
        const path = [];
        let node = target;
        for (let depth = 0; node && depth < 6; depth++, node = node.parentElement) {
            if (node.id) { path.unshift('#' + CSS.escape(node.id)); break; }
            let index = 1;
            for (let sibling = node.previousElementSibling; sibling; sibling = sibling.previousElementSibling) index++;
            path.unshift(node.tagName.toLowerCase() + ':nth-child(' + index + ')');
        }
        const rect = target.getBoundingClientRect();
        const payload = {
            url: location.href.slice(0,4096), selector: path.join(' > ').slice(0,512),
            tag: target.tagName.toLowerCase(),
            text: target.matches('input,textarea,select,[contenteditable]') || target.querySelector('input,textarea,select,[contenteditable]') ? '' : (target.innerText || '').slice(0,512),
            bounds: [rect.x, rect.y, rect.width, rect.height]
        };
        window.ipc.postMessage('browser-element:' + JSON.stringify(payload));
    }, true);
})();

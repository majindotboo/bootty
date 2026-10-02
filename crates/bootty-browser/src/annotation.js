(() => {
    let enabled = false;
    let highlighted = null;
    let oldOutline = '';
    const clear = () => {
        if (highlighted) highlighted.style.outline = oldOutline;
        highlighted = null;
    };
    window.__boottyAnnotate = value => { enabled = Boolean(value); clear(); };
    document.addEventListener('pointermove', event => {
        if (!enabled || !event.isTrusted || !(event.target instanceof Element)) return;
        if (highlighted === event.target) return;
        clear();
        highlighted = event.target;
        oldOutline = highlighted.style.outline;
        highlighted.style.outline = '2px solid Highlight';
    }, true);
    document.addEventListener('click', event => {
        if (!enabled || !event.isTrusted || !(event.target instanceof Element)) return;
        if (event.target.matches('input,textarea,select,[contenteditable]')) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        const target = event.target;
        clear();
        enabled = false;
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
            text: target.querySelector('input,textarea,select,[contenteditable]') ? '' : (target.innerText || '').slice(0,512),
            bounds: [rect.x, rect.y, rect.width, rect.height]
        };
        window.ipc.postMessage('browser-element:' + JSON.stringify(payload));
    }, true);
})();

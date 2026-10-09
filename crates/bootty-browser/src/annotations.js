(() => {
  if (window !== window.top || window.__boottyAnnotations) return;
  let editor = null, editorOutline = null, editorResult = null, editorAvailability = null, editorCleanup = null, editorCapture = null;
  let conversationAvailable = false;
  let pickMode = 'element';
  let pickerShortcut = null;
  let picking = false, picker = null, pickerOutline = null, pickerCleanup = null, pickedElement = null, pickedFocus = null;
  const bounded = (value, length) => [...value.slice(0, length * 2)].slice(0, length).join('');
  const post = value => window.ipc.postMessage('browser-annotation:' + JSON.stringify({ address: location.href, ...value }));
  const restore = element => { if (document.hasFocus() && element?.isConnected && typeof element.focus === 'function') element.focus({ preventScroll: true }); };
  const closeEditor = (restoreFocus = true) => {
    if (editorCleanup) editorCleanup(restoreFocus);
    editorCleanup = null; editor = null; editorOutline = null; editorResult = null; editorAvailability = null; editorCapture = null;
  };
  const stopPicking = (restoreFocus = true) => {
    picking = false;
    if (pickerCleanup) pickerCleanup(restoreFocus);
    pickerCleanup = null; picker = null; pickerOutline = null;
  };
  const close = () => { closeEditor(); stopPicking(); };
  const selector = element => {
    const path = [];
    while (element && path.length < 8) {
      const tag = element.localName;
      if (!tag) break;
      const siblings = element.parentElement ? [...element.parentElement.children].filter(child => child.localName === tag) : [element];
      path.unshift(tag + ':nth-of-type(' + (siblings.indexOf(element) + 1) + ')');
      element = element.parentElement;
    }
    return bounded(path.join(' > '), 512);
  };
  const excerpt = element => {
    const walker = document.createTreeWalker(element, NodeFilter.SHOW_TEXT);
    let text = '', node, visited = 0;
    while ((node = walker.nextNode()) && visited++ < 64 && text.length < 1024) {
      if (node.parentElement?.closest('input,textarea,select,[contenteditable],script,style,template')) continue;
      text += bounded(node.nodeValue || '', 512);
    }
    return bounded(text, 512);
  };
  const pickable = element => element instanceof Element
    && element !== picker && element !== pickerOutline && element !== editor && element !== editorOutline
    && !element.closest('input,textarea,select,[contenteditable]');
  document.addEventListener('keydown', event => {
    if (!picking || !event.isTrusted) return;
    if (!event.ctrlKey && !event.metaKey && !event.altKey && pickerShortcut?.(event.key.toLowerCase())) {
      event.preventDefault(); event.stopImmediatePropagation(); return;
    }
    if (event.key === 'Escape') {
      stopPicking(); post({ action: 'cancel_pick' });
      event.preventDefault(); event.stopImmediatePropagation();
    }
  }, true);
  document.addEventListener('click', event => {
    if (!picking || pickMode !== 'element' || !event.isTrusted || !pickable(event.target)) return;
    event.preventDefault(); event.stopImmediatePropagation();
    pickedElement = event.target; stopPicking(false);
    post({ action: 'pick', anchor: { selector: selector(pickedElement), text: excerpt(pickedElement), tag: bounded(pickedElement.localName, 32) } });
  }, true);
  window.__boottyAnnotations = {
    pick(dark) {
      close(); picking = true; pickedElement = null; pickedFocus = document.activeElement;
      const host = document.createElement('div'); picker = host;
      host.style.cssText = 'position:fixed;z-index:2147483647;bottom:1rem;left:50%;transform:translateX(-50%);max-width:calc(100vw - 2rem);';
      host.style.colorScheme = dark ? 'dark' : 'light';
      const shadow = host.attachShadow({ mode: 'closed' });
      const style = document.createElement('style');
      style.textContent = ':host{color-scheme:light dark}section{display:flex;flex-wrap:wrap;justify-content:center;align-items:center;gap:.75rem;white-space:nowrap;font:13px system-ui;color:CanvasText;background:Canvas;border:1px solid color-mix(in srgb,CanvasText 8%,Canvas);border-radius:.5rem;padding:.375rem .5rem;box-shadow:0 .25rem 1rem color-mix(in srgb,CanvasText 18%,transparent)}button{font:inherit;color:inherit;background:transparent;border:1px solid transparent;border-radius:.25rem;padding:.25rem .5rem}button:focus-visible{outline:2px solid Highlight;outline-offset:2px}';
      const section = document.createElement('section'); section.setAttribute('role', 'toolbar'); section.setAttribute('aria-label', 'Element annotation');
      const label = document.createElement('span'); label.textContent = 'Select an element'; label.setAttribute('role', 'status');
      const modes = document.createElement('div'); modes.style.cssText = 'display:flex;gap:2px';
      const modeButtons = [];
      const layer = document.createElement('div');
      layer.style.cssText = 'position:fixed;inset:0;z-index:2147483645;display:none;touch-action:none;cursor:crosshair';
      const ink = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
      ink.style.cssText = 'width:100%;height:100%;pointer-events:none;overflow:visible';
      layer.append(ink);
      let start = null, points = [];
      const point = event => [Math.round(Math.max(0, Math.min(1000000, event.pageX))), Math.round(Math.max(0, Math.min(1000000, event.pageY)))];
      const trace = () => {
        ink.replaceChildren();
        if (!start || !points.length) return;
        const shape = document.createElementNS(ink.namespaceURI, pickMode === 'region' ? 'rect' : 'polyline');
        const last = points[points.length - 1];
        if (pickMode === 'region') {
          shape.setAttribute('x', Math.min(start[0], last[0]) - scrollX); shape.setAttribute('y', Math.min(start[1], last[1]) - scrollY);
          shape.setAttribute('width', Math.abs(start[0] - last[0])); shape.setAttribute('height', Math.abs(start[1] - last[1]));
          shape.setAttribute('fill', 'color-mix(in srgb, Highlight 12%, transparent)');
        } else { shape.setAttribute('points', points.map(([x,y]) => `${x-scrollX},${y-scrollY}`).join(' ')); shape.setAttribute('fill', 'none'); }
        shape.setAttribute('stroke', 'Highlight'); shape.setAttribute('stroke-width', '2'); shape.setAttribute('stroke-linecap', 'round'); shape.setAttribute('stroke-linejoin', 'round'); ink.append(shape);
      };
      const chooseMode = mode => {
        pickMode = mode; start = null; points = []; ink.replaceChildren(); hovered = null; outline.style.display = 'none';
        layer.style.display = mode === 'element' ? 'none' : 'block';
        label.textContent = mode === 'element' ? 'Select an element' : mode === 'region' ? 'Drag a region' : 'Draw on the page';
        modeButtons.forEach(button => { const active = button.dataset.mode === mode; button.setAttribute('aria-pressed', String(active)); button.style.background = active ? 'color-mix(in srgb, Highlight 20%, Canvas)' : 'transparent'; });
      };
      pickerShortcut = key => { const mode = { v:'element', r:'region', d:'draw' }[key]; if (!mode) return false; chooseMode(mode); return true; };
      const icons = { element: '<path d="m3 3 6 16 2-7 7-2Z"/><path d="m13 13 5 5M17 3v3M21 7h-3"/>', region: '<rect x="4" y="4" width="16" height="16" rx="2" stroke-dasharray="3 3"/>', draw: '<path d="m4 16-1 5 5-1L20 8l-4-4ZM14 6l4 4"/>' };
      for (const [mode, title] of [['element','Element'],['region','Region'],['draw','Draw']]) {
        const button = document.createElement('button'); button.dataset.mode = mode; button.title = `${title} (${mode === 'element' ? 'V' : mode === 'region' ? 'R' : 'D'})`; button.setAttribute('aria-label', title);
        // Only fixed product icons enter this isolated shadow tree; page data is text-only.
        button.innerHTML = `<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" aria-hidden="true">${icons[mode]}</svg>`;
        button.onclick = () => chooseMode(mode); modeButtons.push(button); modes.append(button);
      }
      layer.addEventListener('pointerdown', event => { if (!event.isTrusted || event.button !== 0) return; event.preventDefault(); start = point(event); points = [start]; layer.setPointerCapture(event.pointerId); trace(); });
      layer.addEventListener('pointermove', event => { if (!event.isTrusted || !start) return; const next = point(event); if (pickMode === 'region') points = [start, next]; else if (points.length < 512 && (points.at(-1)[0] !== next[0] || points.at(-1)[1] !== next[1])) points.push(next); trace(); });
      layer.addEventListener('pointerup', event => {
        if (!event.isTrusted || !start) return;
        const end = point(event); let selection;
        if (pickMode === 'region') { selection = { kind:'region', x:Math.min(start[0],end[0]), y:Math.min(start[1],end[1]), width:Math.abs(start[0]-end[0]), height:Math.abs(start[1]-end[1]) }; if (selection.width < 2 || selection.height < 2) { start = null; ink.replaceChildren(); return; } }
        else { if (points.length < 512 && (points.at(-1)[0] !== end[0] || points.at(-1)[1] !== end[1])) points.push(end); if (points.length < 2) { start = null; points = []; ink.replaceChildren(); return; } selection = { kind:'drawing', points }; }
        const mode = pickMode; stopPicking(false);
        post({ action:'pick', anchor:{ selector:'', text:mode === 'region' ? 'Selected page region' : 'Drawing on the page', tag:mode === 'region' ? 'region' : 'drawing', selection } });
      });
      layer.addEventListener('pointercancel', () => { start = null; points = []; ink.replaceChildren(); });
      const cancel = document.createElement('button'); cancel.textContent = 'Cancel'; cancel.title = 'Cancel element selection (Esc)';
      cancel.onclick = () => { stopPicking(); post({ action: 'cancel_pick' }); };
      const outline = document.createElement('div'); pickerOutline = outline;
      outline.setAttribute('aria-hidden', 'true');
      outline.style.cssText = 'position:fixed;z-index:2147483646;pointer-events:none;display:none;box-sizing:border-box;border:2px solid Highlight;border-radius:2px;';
      let hovered = null;
      const position = () => {
        if (!hovered?.isConnected) { outline.style.display = 'none'; return; }
        const rect = hovered.getBoundingClientRect();
        outline.style.display = 'block'; outline.style.left = rect.left + 'px'; outline.style.top = rect.top + 'px';
        outline.style.width = rect.width + 'px'; outline.style.height = rect.height + 'px';
      };
      const move = event => { if (pickMode === 'element' && event.isTrusted) { hovered = pickable(event.target) ? event.target : null; position(); } };
      const cursor = document.documentElement.style.cursor;
      document.documentElement.style.cursor = 'crosshair';
      const observer = new MutationObserver(() => { if (picker === host && !host.isConnected) stopPicking(false); });
      document.addEventListener('pointermove', move, true);
      const layout = () => { position(); trace(); };
      window.addEventListener('scroll', layout, { passive: true, capture: true }); window.addEventListener('resize', layout, { passive: true });
      pickerCleanup = restoreFocus => {
        const hadFocus = document.activeElement === host;
        observer.disconnect(); pickerShortcut = null;
        document.removeEventListener('pointermove', move, true);
        window.removeEventListener('scroll', layout, true); window.removeEventListener('resize', layout);
        document.documentElement.style.cursor = cursor; host.remove(); outline.remove(); layer.remove();
        if (restoreFocus && hadFocus) restore(pickedFocus);
      };
      section.append(modes, label, cancel); shadow.append(style, section); document.documentElement.append(layer, outline, host); chooseMode('element'); cancel.focus();
      observer.observe(document.documentElement, { childList: true });
    },
    edit(record, dark, available) {
      conversationAvailable = Boolean(available);
      closeEditor(); stopPicking(false);
      const previousFocus = pickedFocus?.isConnected ? pickedFocus : document.activeElement;
      pickedFocus = null;
      const host = document.createElement('div'); editor = host;
      host.style.cssText = 'position:fixed;z-index:2147483647;max-width:calc(100vw - 1rem);width:360px;';
      host.style.colorScheme = dark ? 'dark' : 'light';
      const shadow = host.attachShadow({ mode: 'closed' });
      const style = document.createElement('style');
      style.textContent = ':host{color-scheme:light dark}section{font:14px system-ui;color:CanvasText;background:Canvas;border:1px solid color-mix(in srgb,CanvasText 8%,Canvas);border-radius:.75rem;overflow:hidden;box-shadow:0 .5rem 1.5rem color-mix(in srgb,CanvasText 20%,transparent)}.composer{display:flex;align-items:flex-start;gap:.5rem;padding:.5rem}textarea{display:block;box-sizing:border-box;flex:1;min-width:0;height:2rem;min-height:2rem;max-height:6rem;line-height:1.25rem;padding:.375rem 0;font:inherit;color:inherit;background:transparent;border:0;border-bottom:1px solid transparent;outline:none;resize:none;overflow-y:hidden}textarea:focus{border-bottom-color:Highlight}button{box-sizing:border-box;height:2rem;flex-shrink:0;font:inherit;padding:0 .75rem;color:inherit;background:transparent;border:1px solid transparent;border-radius:.375rem}button:disabled{opacity:.5}button:focus-visible{outline:2px solid Highlight;outline-offset:-2px}.adjust{width:2rem;padding:0;background:color-mix(in srgb,CanvasText 7%,Canvas);display:grid;place-items:center}.adjust:hover{background:color-mix(in srgb,CanvasText 12%,Canvas)}.adjust-icon{display:grid;gap:3px;width:15px}.adjust-icon i{position:relative;height:1px;background:currentColor}.adjust-icon i:after{content:"";position:absolute;left:3px;top:-2px;width:3px;height:3px;border:1px solid currentColor;border-radius:50%;background:Canvas}.adjust-icon i:nth-child(2):after{left:9px}.adjust-icon i:nth-child(3):after{left:5px}.attach{background:Highlight;color:HighlightText}.details{padding:.5rem .75rem;border-top:1px solid color-mix(in srgb,CanvasText 8%,Canvas);font-size:12px;overflow-wrap:anywhere}.details strong,.details code{display:block;margin-bottom:.375rem}.details code{opacity:.7}[hidden]{display:none!important}[role=status]:empty{display:none}[role=status]{padding:0 .5rem .5rem;font-size:12px;overflow-wrap:anywhere}';
      const section = document.createElement('section'); section.setAttribute('role', 'dialog'); section.setAttribute('aria-label', 'Page annotation');
      const composer = document.createElement('div'); composer.className = 'composer';
      const adjust = document.createElement('button'); adjust.className = 'adjust'; adjust.title = 'Show selected element'; adjust.setAttribute('aria-label', adjust.title); adjust.setAttribute('aria-expanded', 'false');
      const icon = document.createElement('span'); icon.className = 'adjust-icon'; icon.setAttribute('aria-hidden', 'true');
      icon.append(document.createElement('i'), document.createElement('i'), document.createElement('i')); adjust.append(icon);
      const text = document.createElement('textarea'); text.rows = 1; text.maxLength = 1024; text.setAttribute('aria-label', 'Annotation'); text.placeholder = 'Describe the change…'; text.value = record.draft === null ? record.note : record.draft;
      const details = document.createElement('div'); details.className = 'details'; details.hidden = true;
      const context = document.createElement('strong'); context.textContent = record.anchor.text.trim() || record.anchor.tag;
      const path = document.createElement('code'); path.textContent = record.anchor.selection ? JSON.stringify(record.anchor.selection) : record.anchor.selector;
      const status = document.createElement('div'); status.setAttribute('role', 'status');
      const cancel = document.createElement('button'); cancel.textContent = 'Cancel'; cancel.title = 'Cancel annotation (Esc)';
      const save = document.createElement('button'); save.className = 'attach'; save.textContent = 'Attach'; save.title = 'Attach (Enter)'; save.disabled = !text.value.trim();
      let saving = false, focusOwned = true, failure = null;
      const trackFocus = event => { if (event.isTrusted) focusOwned = event.target === host; };
      document.addEventListener('pointerdown', trackFocus, true);
      document.addEventListener('focusin', trackFocus, true);
      const emit = action => post({ action, id: String(record.id), ...(action === 'cancel' ? {} : { note: bounded(text.value, 1024) }) });
      const resizeText = () => { text.style.height = 'auto'; text.style.height = Math.min(text.scrollHeight, 96) + 'px'; text.style.overflowY = text.scrollHeight > 96 ? 'auto' : 'hidden'; };
      text.addEventListener('input', () => { if (!saving) { emit('draft'); resizeText(); refreshAvailability(); } });
      adjust.onclick = () => { details.hidden = !details.hidden; adjust.setAttribute('aria-expanded', String(!details.hidden)); adjust.title = details.hidden ? 'Show selected element' : 'Hide selected element'; adjust.setAttribute('aria-label', adjust.title); position(); };
      cancel.onclick = () => { emit('cancel'); closeEditor(); };
      save.onclick = () => { refreshAvailability(); if (!save.disabled) { saving = true; if (focusOwned) text.focus({ preventScroll: true }); save.disabled = true; adjust.disabled = true; text.readOnly = true; status.textContent = 'Saving'; emit('save'); position(); } };
      section.addEventListener('keydown', event => {
        if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); cancel.click(); }
        if (event.key === 'Enter' && (!event.shiftKey || event.ctrlKey || event.metaKey)) { event.preventDefault(); event.stopPropagation(); save.click(); }
      });
      const anchor = (() => {
        if (record.anchor.selection) return null;
        // A changed document can invalidate an anchor. Do not attach a note to an ambiguous element.
        try { const matches = document.querySelectorAll(record.anchor.selector); return matches.length === 1 && (!pickedElement || matches[0] === pickedElement) ? matches[0] : null; } catch { return null; }
      })();
      const anchorAvailable = () => {
        if (record.anchor.selection) return true;
        if (!anchor?.isConnected) return false;
        try { const matches = document.querySelectorAll(record.anchor.selector); return matches.length === 1 && matches[0] === anchor; } catch { return false; }
      };
      let captureIntent = null;
      const captureContext = () => {
        if (editor !== host || location.href !== record.address || !anchorAvailable()) return null;
        const visualViewport = window.visualViewport;
        const viewport = visualViewport ? { x:visualViewport.pageLeft, y:visualViewport.pageTop, width:visualViewport.width, height:visualViewport.height } : { x:scrollX, y:scrollY, width:innerWidth, height:innerHeight };
        const geometry = record.anchor.selection;
        let selection;
        if (geometry?.kind === 'region') selection = { x:geometry.x, y:geometry.y, width:geometry.width, height:geometry.height };
        else if (geometry?.kind === 'drawing') {
          const xs = geometry.points.map(point=>point[0]), ys = geometry.points.map(point=>point[1]);
          selection = { x:Math.min(...xs), y:Math.min(...ys), width:Math.max(1,Math.max(...xs)-Math.min(...xs)), height:Math.max(1,Math.max(...ys)-Math.min(...ys)) };
        } else { const rect = anchor.getBoundingClientRect(); selection = { x:rect.left+scrollX, y:rect.top+scrollY, width:rect.width, height:rect.height }; }
        return { document:window.__boottyCredentials?.document(), viewport, selection };
      };
      editorCapture = {
        prepare(id, intent) {
          if (String(record.id) !== id || captureIntent !== null || !saving) return null;
          const context = captureContext(); if (!context) return null;
          captureIntent = intent; host.style.opacity = '0'; host.style.pointerEvents = 'none';
          // Keep rendered marks and focus; wait for the hidden composer to reach the compositor.
          requestAnimationFrame(() => requestAnimationFrame(() => {
            const current = editor === host && captureIntent === intent ? captureContext() : null;
            window.ipc.postMessage('browser-annotation-capture:' + JSON.stringify({ intent, context:current }));
          }));
          return true;
        },
        context(id, intent) { return String(record.id) === id && captureIntent === intent ? captureContext() : null; },
        finish(id, intent) { if (String(record.id) === id && captureIntent === intent) { captureIntent = null; host.style.opacity = ''; host.style.pointerEvents = ''; } }
      };
      const outline = document.createElement('div'); editorOutline = outline;
      outline.setAttribute('aria-hidden', 'true');
      outline.style.cssText = 'position:fixed;z-index:2147483646;pointer-events:none;box-sizing:border-box;border:2px solid Highlight;border-radius:2px;';
      if (record.anchor.selection?.kind === 'drawing') {
        outline.style.border = '0';
        const points = record.anchor.selection.points, left = Math.min(...points.map(p=>p[0])), top = Math.min(...points.map(p=>p[1]));
        const svg = document.createElementNS('http://www.w3.org/2000/svg','svg'); svg.style.cssText='width:100%;height:100%;overflow:visible';
        const line = document.createElementNS(svg.namespaceURI,'polyline'); line.setAttribute('points',points.map(([x,y])=>`${x-left},${y-top}`).join(' ')); line.setAttribute('fill','none'); line.setAttribute('stroke','Highlight'); line.setAttribute('stroke-width','2'); line.setAttribute('stroke-linecap','round'); line.setAttribute('stroke-linejoin','round'); svg.append(line); outline.append(svg);
      }
      const position = () => {
        if (editor !== host) return;
        if (location.href !== record.address || (anchor && !anchor.isConnected)) { closeEditor(false); return; }
        const geometry = record.anchor.selection;
        const selectedPoints = geometry?.kind === 'drawing' ? geometry.points : [];
        const region = geometry?.kind === 'region' ? geometry : selectedPoints.length ? { x:Math.min(...selectedPoints.map(p=>p[0])), y:Math.min(...selectedPoints.map(p=>p[1])), width:Math.max(1,Math.max(...selectedPoints.map(p=>p[0]))-Math.min(...selectedPoints.map(p=>p[0]))), height:Math.max(1,Math.max(...selectedPoints.map(p=>p[1]))-Math.min(...selectedPoints.map(p=>p[1]))) } : null;
        const rect = region ? { left:region.x-scrollX, top:region.y-scrollY, width:region.width, height:region.height, bottom:region.y+region.height-scrollY } : anchor?.isConnected ? anchor.getBoundingClientRect() : { left:12, top:12, bottom:12 };
        outline.style.display = anchor || region ? 'block' : 'none'; outline.style.left = rect.left + 'px'; outline.style.top = rect.top + 'px';
        outline.style.width = rect.width + 'px'; outline.style.height = rect.height + 'px';
        const below = rect.bottom + 8;
        const top = below + host.offsetHeight <= innerHeight - 8 ? below : rect.top - host.offsetHeight - 8;
        host.style.left = Math.max(8, Math.min(rect.left, innerWidth - host.offsetWidth - 8)) + 'px';
        host.style.top = Math.max(8, Math.min(top, innerHeight - host.offsetHeight - 8)) + 'px';
      };
      const refreshAvailability = () => {
        const available = anchorAvailable();
        save.disabled = saving || !available || !conversationAvailable || !text.value.trim();
        const message = available ? failure || '' : 'This element is no longer available';
        if (!saving && status.textContent !== message) status.textContent = message;
        save.title = conversationAvailable ? 'Attach (Enter)' : 'Select an agent to attach';
        position();
      };
      editorAvailability = refreshAvailability;
      editorResult = (id, error) => {
        if (String(record.id) !== id) return;
        if (error === null) closeEditor();
        else { saving = false; failure = error; refreshAvailability(); cancel.disabled = false; adjust.disabled = false; text.readOnly = false; if (focusOwned && document.hasFocus()) text.focus(); position(); }
      };
      const observer = new MutationObserver(() => { if (editor === host) { if (!host.isConnected || (anchor && !anchor.isConnected)) closeEditor(false); else refreshAvailability(); } });
      editorCleanup = restoreFocus => {
        const hadFocus = focusOwned && document.activeElement === host;
        observer.disconnect();
        document.removeEventListener('pointerdown', trackFocus, true); document.removeEventListener('focusin', trackFocus, true);
        window.removeEventListener('scroll', position, true); window.removeEventListener('resize', position); host.remove(); outline.remove();
        window.removeEventListener('hashchange', position); window.removeEventListener('popstate', position);
        if (restoreFocus && hadFocus) restore(previousFocus);
      };
      details.append(context, path, cancel); composer.append(adjust, text, save); section.append(composer, details, status); shadow.append(style, section); document.documentElement.append(outline, host);
      resizeText(); refreshAvailability(); if (editor !== host) return; text.focus();
      observer.observe(document.documentElement, { childList: true, subtree: true });
      window.addEventListener('scroll', position, { passive: true, capture: true }); window.addEventListener('resize', position, { passive: true });
      window.addEventListener('hashchange', position); window.addEventListener('popstate', position);
    },
    conversation(available) { conversationAvailable = Boolean(available); if (editorAvailability) editorAvailability(); },
    result(id, error) { if (editorResult) editorResult(id, error); },
    prepareCapture(id, intent) { if (editorCapture?.prepare(id,intent) !== true) window.ipc.postMessage('browser-annotation-capture:' + JSON.stringify({ intent, context:null })); },
    captureContext(id, intent) { return editorCapture?.context(id,intent) ?? null; },
    finishCapture(id, intent) { editorCapture?.finish(id,intent); },
    close
  };
})();

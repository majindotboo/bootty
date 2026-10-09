([token, address]) => {
  try {
    if (window !== window.top || window.__boottyCredentials?.document() !== token || location.href !== address || !document.body) return null;
    const clip = (value, limit) => {
      let result = '';
      for (const character of value) {
        if (result.length + character.length > limit) break;
        result += character;
      }
      return result;
    };
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const range = document.createRange();
    let text = '', truncated = false, visited = 0;
    for (let node = walker.nextNode(); node; node = walker.nextNode()) {
      // Bound traversal too: upgrade to an accessibility tree for interactive browser tools.
      if (++visited > 10000) { truncated = true; break; }
      const parent = node.parentElement;
      if (!parent || parent.isContentEditable || parent.closest('input,textarea,select,script,style,template,[hidden]') || getComputedStyle(parent).visibility !== 'visible') continue;
      range.selectNodeContents(node);
      if (!range.getClientRects().length) continue;
      const remaining = 16384 - text.length;
      const part = clip(node.data, remaining);
      text += part;
      if (part.length < node.data.length || text.length >= 16384) { truncated = true; break; }
      text += '\n';
    }
    range.detach();
    return { document: token, address, title: clip(document.title, 256), text, truncated };
  } catch { return null; }
}

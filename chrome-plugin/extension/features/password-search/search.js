function hostFromEntry(file) {
  const parts = file.replace(/\.gpg$/u, "").split("/").reverse();
  return parts.find((part) => /^[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+(?::\d+)?$/iu.test(part)) || null;
}

function hostMatches(entryHost, currentHost, currentPort = "") {
  if (!entryHost || !currentHost) return false;
  const [hostname, port] = entryHost.toLowerCase().split(":");
  return (hostname === currentHost || currentHost.endsWith(`.${hostname}`)) && (!port || port === currentPort);
}

function fuzzyScore(text, query) {
  if (!query) return 0;
  let position = 0;
  let score = 0;
  for (const character of query.toLowerCase()) {
    const index = text.toLowerCase().indexOf(character, position);
    if (index < 0) return -Infinity;
    score += 8 - Math.min(index - position, 7);
    position = index + 1;
  }
  return score;
}

export function rankEntries(files, { query = "", host = "", port = "", domainOnly = true } = {}) {
  const words = query.trim().split(/\s+/u).filter(Boolean);
  const fuzzy = words.shift() || "";
  return files.map((file) => {
    const lower = file.toLowerCase();
    const entryHost = hostFromEntry(file);
    const current = hostMatches(entryHost, host, port);
    const substringScore = words.every((word) => lower.includes(word.toLowerCase())) ? words.length * 30 : -Infinity;
    const fuzzyScoreValue = fuzzyScore(lower, fuzzy);
    return { file, entryHost, current, score: (current ? 1000 : 0) + substringScore + fuzzyScoreValue };
  }).filter((entry) => (!domainOnly || !host || entry.current) && entry.score > -Infinity)
    .sort((a, b) => b.score - a.score || a.file.localeCompare(b.file));
}

export { hostFromEntry, hostMatches };

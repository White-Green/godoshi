(() => {
  'use strict';

  const form = document.querySelector('[data-manuscript-upload]');
  if (!form) return;
  // Sort each newly selected group, preserving the order already chosen by the author.
  const compareNames = new Intl.Collator('ja', {numeric: true}).compare;
  const error = form.querySelector('[data-upload-error]');
  const showError = message => {
    error.textContent = message;
    error.classList.toggle('d-none', !message);
  };
  const input = form.querySelector('[data-add-files]');
  const add = form.querySelector('[data-add-pages]');
  const list = form.querySelector('[data-page-list]');
  const template = form.querySelector('[data-page-template]');
  const summary = form.querySelector('[data-page-summary]');
  const status = form.querySelector('[data-edit-status]');
  const recovery = form.querySelector('[data-upload-recovery]');
  const reload = form.querySelector('[data-reload-manuscript]');
  const maxFiles = Number(form.dataset.maxFiles);
  const isImage = name => /\.(png|jpe?g|webp|tiff?|bmp|gif)$/i.test(name);
  let pages = Array.from(list.querySelectorAll('[data-existing-page]'), row => ({
    id: `existing:${row.dataset.index}`, existing: Number(row.dataset.index), name: row.dataset.name, url: row.dataset.url,
  }));
  let revision = Number(form.dataset.revision);
  let nextId = 0;
  let busy = false;
  let blocked = false;
  let uploading;
  let message = pages.length ? '保存済み' : '';
  const pending = () => pages.filter(page => page.file);
  function additionUnavailable() {
    if (blocked) return '最新の状態を読み込んでからファイルを追加してください。';
    if (busy) return '保存が完了してからファイルを追加してください。';
    if (pending().length) return '未送信のファイルを再送信するか、追加を中止してください。';
    return '';
  }
  const dialog = form.querySelector('[data-preview-dialog]');
  const previewImage = dialog.querySelector('[data-preview-image]');
  const rows = new Map();
  let previewOpener;
  dialog.querySelector('[data-close-preview]').addEventListener('click', () => dialog.close());
  dialog.addEventListener('close', () => { previewImage.removeAttribute('src'); previewOpener?.focus(); });
  dialog.addEventListener('click', event => { if (event.target === dialog) dialog.close(); });

  function createRow(page) {
    const row = template.content.firstElementChild.cloneNode(true);
    row.dataset.pageId = page.id;
    const find = selector => row.querySelector(selector);
    const item = {
      row, thumbnail: find('[data-preview-page]'), image: find('img'),
      fallback: find('[data-preview-fallback]'), open: find('a'),
      number: find('strong'), badge: find('.badge'),
      up: find('[data-action="up"]'), down: find('[data-action="down"]'), remove: find('[data-action="remove"]'),
    };
    const {thumbnail, image, fallback} = item;
    image.addEventListener('error', () => { thumbnail.classList.add('d-none'); fallback.classList.remove('d-none'); });
    image.addEventListener('load', () => { thumbnail.classList.remove('d-none'); fallback.classList.add('d-none'); });
    if (isImage(page.name)) image.src = page.url;
    else { thumbnail.classList.add('d-none'); fallback.classList.remove('d-none'); }
    thumbnail.addEventListener('click', () => {
      const index = pages.indexOf(page);
      dialog.querySelector('[data-preview-title]').textContent = `${index + 1}ページ目`;
      dialog.querySelector('[data-preview-name]').textContent = page.name;
      previewImage.src = page.blobUrl ?? page.url;
      previewImage.alt = `${index + 1}ページ目のプレビュー`;
      previewOpener = thumbnail;
      dialog.showModal();
    });
    function move(offset) {
      const index = pages.indexOf(page);
      const next = [...pages];
      [next[index], next[index + offset]] = [next[index + offset], next[index]];
      updatePages(next, page.id, offset < 0 ? 'up' : 'down');
    }
    item.up.addEventListener('click', () => move(-1));
    item.down.addEventListener('click', () => move(1));
    item.remove.addEventListener('click', () => {
      const index = pages.indexOf(page);
      if (!window.confirm(`${index + 1}ページ目を削除しますか？`)) return;
      const next = pages.filter(item => item !== page);
      updatePages(next, next[Math.min(index, next.length - 1)]?.id, 'remove');
    });
    return item;
  }

  function render(focusId, focusAction) {
    const waiting = pending().length;
    const locked = busy || blocked || waiting > 0;
    summary.textContent = `${pages.length - waiting}ファイル${waiting ? `・追加待ち ${waiting}件` : ''}`;
    status.textContent = message;
    status.className = `small text-break mb-0 ${busy ? 'text-primary' : 'text-body-secondary'}`;
    const empty = form.querySelector('[data-empty-pages]');
    empty.classList.toggle('d-none', pages.length > 0);
    list.classList.toggle('d-none', pages.length === 0);
    recovery.classList.toggle('d-none', busy || !waiting);
    recovery.querySelector('[data-retry-uploads]').disabled = blocked;
    reload.classList.toggle('d-none', !blocked);
    input.disabled = locked;
    add.disabled = locked;
    if (add.disabled) add.classList.remove('active');
    for (const [id, item] of rows) {
      if (!pages.some(page => page.id === id)) { item.row.remove(); rows.delete(id); }
    }
    pages.forEach((page, index) => {
      if (!rows.has(page.id)) rows.set(page.id, createRow(page));
      const item = rows.get(page.id);
      const label = `${index + 1}ページ目`;
      item.number.textContent = label;
      item.badge.classList.toggle('d-none', !page.file);
      item.badge.textContent = page === uploading ? '送信中' : busy ? '待機中' : '未送信';
      // Open files through the server's content-type and sandbox policy, never as local HTML blobs.
      item.open.classList.toggle('d-none', Boolean(page.file));
      if (!page.file) item.open.href = page.url;
      // Keep decoded thumbnails when their page moves; refresh still-unloaded URLs to the current revision.
      if (isImage(page.name) && !page.blobUrl && item.image.getAttribute('src') !== page.url && (!item.image.complete || !item.image.naturalWidth)) {
        item.image.src = page.url;
      }
      item.thumbnail.setAttribute('aria-label', `${label}のプレビューを拡大`);
      item.image.alt = `${label}のプレビュー`;
      item.up.disabled = locked || index === 0;
      item.down.disabled = locked || index === pages.length - 1;
      item.remove.disabled = locked;
      for (const [button, text] of [[item.up, '前へ'], [item.down, '次へ'], [item.remove, '削除']]) {
        button.setAttribute('aria-label', `${label}：${text}`);
        button.title = text;
      }
      list.append(item.row);
    });
    if (focusId) {
      const row = rows.get(focusId)?.row;
      const preferred = row?.querySelector(`[data-action="${focusAction}"]:not(:disabled)`);
      (preferred ?? row?.querySelector('button[data-action]:not(:disabled)') ?? add).focus();
    }
  }

  async function result(response) {
    if (!response.ok) {
      const error = new Error(response.status === 413 ? 'ファイルサイズがサーバーのアップロード上限を超えています。' : await response.text());
      error.status = response.status;
      throw error;
    }
    return response.json();
  }

  function applySaved(response, savedPages) {
    if (response.revision !== revision + 1 || !Array.isArray(response.filenames)
      || response.filenames.length !== savedPages.length || response.filenames.some(name => typeof name !== 'string')) {
      throw new SyntaxError('保存結果を確認できませんでした。最新の状態を読み込んでください。');
    }
    revision = response.revision;
    savedPages.forEach((page, index) => {
      page.existing = index;
      page.name = response.filenames[index];
      page.url = `${form.dataset.sourceUrl}/${index}?revision=${revision}`;
      delete page.file;
    });
    return response.build_requested === false ? '保存済み（完成稿の生成依頼に失敗しました）' : '保存済み';
  }

  function failed(reason) {
    blocked = reason.status === 409 || reason instanceof TypeError || reason instanceof SyntaxError;
    showError(reason instanceof TypeError ? '通信が途切れ、反映状況を確認できません。最新の状態を読み込んでください。' : reason.message);
    message = blocked ? '最新の状態を確認してください' : pending().length
      ? `${pages.length - pending().length}件保存済み・${pending().length}件未送信` : '変更を保存できませんでした';
  }

  async function updatePages(next, focusId, focusAction) {
    if (busy || blocked || pending().length) return;
    busy = true;
    message = '変更を保存中…';
    showError('');
    render();
    try {
      const response = await result(await fetch(form.dataset.commitUrl, {
        method: 'POST', headers: {'Content-Type': 'application/json'},
        body: JSON.stringify({revision, pages: next.map(page => page.existing)}),
      }));
      message = applySaved(response, next);
      pages.filter(page => !next.includes(page) && page.blobUrl).forEach(page => URL.revokeObjectURL(page.blobUrl));
      pages = next;
    } catch (reason) { failed(reason); }
    busy = false;
    render(focusId, focusAction);
    if (!pages.length) add.focus();
  }

  async function uploadPending() {
    if (busy || blocked) return;
    const additions = pending();
    if (!additions.length) return;
    busy = true;
    showError('');
    try {
      for (const [index, page] of additions.entries()) {
        uploading = page;
        message = `ファイルを追加中（${index + 1}/${additions.length}）：${page.name}`;
        render();
        const url = new URL(form.dataset.filesUrl, window.location.href);
        url.searchParams.set('filename', page.name);
        url.searchParams.set('revision', revision);
        const response = await result(await fetch(url, {
          method: 'POST', headers: {'Content-Type': 'application/octet-stream'}, body: page.file,
        }));
        message = applySaved(response, [...pages.filter(item => !item.file), page]);
      }
    } catch (reason) { failed(reason); }
    uploading = undefined;
    busy = false;
    render();
  }

  function addFiles(selected) {
    const reason = additionUnavailable();
    if (reason) return showError(reason);
    const files = Array.from(selected).sort((a, b) => compareNames(a.name, b.name));
    if (!files.length) return;
    if (pages.length + files.length > maxFiles) return showError('原稿は100ファイル以下にしてください。');
    pages.push(...files.map(file => {
      // Non-image files are opened through FileResponse after saving, using its safe content type.
      const url = isImage(file.name) ? URL.createObjectURL(file) : undefined;
      return {id: `new:${nextId++}`, name: file.name, file, url, blobUrl: url};
    }));
    uploadPending();
  }
  add.addEventListener('click', () => input.click());
  input.addEventListener('change', () => { addFiles(input.files); input.value = ''; });
  // Files are available only on drop; use their advertised type while dragging.
  const isFileDrag = event => Array.from(event.dataTransfer?.types ?? []).includes('Files');
  let dragDepth = 0;
  function resetDrag() {
    dragDepth = 0;
    add.classList.remove('active');
  }
  add.addEventListener('dragenter', event => {
    if (!isFileDrag(event)) return;
    event.preventDefault();
    dragDepth++;
    if (!additionUnavailable()) add.classList.add('active');
  });
  add.addEventListener('dragover', event => {
    if (!isFileDrag(event)) return;
    event.preventDefault();
    event.dataTransfer.dropEffect = additionUnavailable() ? 'none' : 'copy';
    add.classList.toggle('active', !additionUnavailable());
  });
  add.addEventListener('dragleave', () => {
    if (dragDepth > 0 && --dragDepth === 0) resetDrag();
  });
  add.addEventListener('drop', event => {
    resetDrag();
    if (!isFileDrag(event)) return;
    event.preventDefault();
    if (Array.from(event.dataTransfer.items).some(item => item.webkitGetAsEntry?.()?.isDirectory)) {
      showError('フォルダーではなく、原稿ファイルを選択してドロップしてください。');
      return;
    }
    addFiles(event.dataTransfer.files);
  });
  // Ignore files outside the picker, and prevent the browser from opening them.
  document.addEventListener('dragover', event => {
    if (!isFileDrag(event)) return;
    event.preventDefault();
    if (!add.contains(event.target)) event.dataTransfer.dropEffect = 'none';
  });
  document.addEventListener('drop', event => {
    resetDrag();
    if (isFileDrag(event)) event.preventDefault();
  });
  document.addEventListener('dragend', resetDrag);
  window.addEventListener('blur', resetDrag);
  recovery.querySelector('[data-retry-uploads]').addEventListener('click', uploadPending);
  recovery.querySelector('[data-cancel-uploads]').addEventListener('click', () => {
    if (busy) return;
    pending().filter(page => page.blobUrl).forEach(page => URL.revokeObjectURL(page.blobUrl));
    pages = pages.filter(page => !page.file);
    if (!blocked) { showError(''); message = pages.length ? '保存済み' : ''; }
    render();
  });
  window.addEventListener('beforeunload', event => {
    if (busy || pending().length) { event.preventDefault(); event.returnValue = ''; }
  });
  form.addEventListener('submit', event => event.preventDefault());
  list.replaceChildren();
  render();
})();

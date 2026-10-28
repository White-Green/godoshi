(() => {
    "use strict";

    const form = document.querySelector("[data-schema-form]");
    if (!form) return;

    const definition = form.querySelector("[data-schema-definition]");
    const source = form.querySelector("[data-schema-source]");
    const editor = form.querySelector("[data-schema-editor]");
    const fields = form.querySelector("[data-schema-fields]");
    const addButton = form.querySelector("[data-add-field]");
    const error = form.querySelector("[data-schema-error]");
    const status = form.querySelector("[data-schema-status]");
    const template = document.querySelector("#schema-field-template");
    const encoder = new TextEncoder();
    const maxFields = 64;
    let nextId = 0;

    const control = (card, name) => card.querySelector(`[data-field="${name}"]`);
    const cards = () => Array.from(fields.children);
    const showError = (message) => {
        error.textContent = message;
        error.hidden = !message;
    };

    function updateType(card) {
        const isFile = control(card, "type").value === "file";
        card.querySelector("[data-text-options]").hidden = isFile;
        card.querySelector("[data-file-options]").hidden = !isFile;
        control(card, "max_length").disabled = isFile;
        control(card, "accept").disabled = !isFile;
    }

    function refresh() {
        const current = cards();
        current.forEach((card, index) => {
            const label = control(card, "label").value.trim() || "新しい項目";
            card.setAttribute("aria-label", `${index + 1}. ${label}`);
            card.querySelector("[data-move-up]").disabled = index === 0;
            card.querySelector("[data-move-down]").disabled = index === current.length - 1;
            for (const [action, text] of [["move-up", "上へ移動"], ["move-down", "下へ移動"], ["remove-field", "削除"]]) {
                card.querySelector(`[data-${action}]`).setAttribute("aria-label", `${label}を${text}`);
            }
        });
        form.querySelector("[data-field-count]").textContent = `${current.length} / ${maxFields}`;
        form.querySelector("[data-schema-empty]").hidden = current.length !== 0;
        addButton.disabled = current.length >= maxFields;
    }

    function validate() {
        const current = cards();
        const keys = current.map(card => control(card, "key").value);
        current.forEach(card => {
            for (const input of card.querySelectorAll("[data-field]")) input.setCustomValidity("");
            const key = control(card, "key");
            if (keys.filter(value => value === key.value).length > 1) key.setCustomValidity("識別キーが他の項目と重複しています。");
            const label = control(card, "label");
            if (!label.value.trim()) label.setCustomValidity("項目名を入力してください。");
            for (const [name, limit] of [["label", 1024], ["description", 4096], ["accept", 1024]]) {
                const input = control(card, name);
                if (!input.disabled && encoder.encode(input.value).length > limit) {
                    input.setCustomValidity(`入力内容が長すぎます。UTF-8で${limit}バイト以内にしてください。`);
                }
            }
        });
    }

    function appendField(field) {
        const card = template.content.firstElementChild.cloneNode(true);
        const id = `schema-field-${++nextId}`;
        for (const name of ["key", "label", "description", "accept", "max_length"]) {
            control(card, name).value = field[name] ?? "";
        }
        control(card, "type").value = field.type === "file" ? "file" : field.multiline ? "multiline" : "text";
        for (const name of ["key", "accept"]) {
            const help = card.querySelector(`[data-${name}-help]`);
            help.id = `${id}-${name}-help`;
            control(card, name).setAttribute("aria-describedby", help.id);
        }
        updateType(card);
        fields.append(card);
        return card;
    }

    // Use the escaped textarea as the data source: user text never becomes HTML or executable script.
    // Leave the original form available if the data cannot be represented without loss.
    try {
        const schema = JSON.parse(definition.value);
        if (!schema || !Array.isArray(schema.fields) || schema.fields.length > maxFields || Object.keys(schema).some(key => key !== "fields")) throw new Error();
        for (const field of schema.fields) {
            if (!field || typeof field.key !== "string" || typeof field.label !== "string" ||
                (field.description !== undefined && typeof field.description !== "string")) throw new Error();
            const allowed = ["key", "label", "description", "type"];
            if (field.type === "string") {
                allowed.push("multiline", "max_length");
                if (field.multiline !== undefined && typeof field.multiline !== "boolean") throw new Error();
                if (field.max_length != null && !Number.isSafeInteger(field.max_length)) throw new Error();
            } else if (field.type === "file") {
                allowed.push("accept");
                if (field.accept != null && typeof field.accept !== "string") throw new Error();
            } else throw new Error();
            if (Object.keys(field).some(key => !allowed.includes(key))) throw new Error();
        }
        schema.fields.forEach(appendField);
    } catch {
        showError("項目をフォームに読み込めませんでした。入力内容は下のJSON欄に保持しています。内容を修正して保存してください。");
        return;
    }

    addButton.addEventListener("click", () => {
        if (cards().length >= maxFields) return;
        const used = new Set(cards().map(card => control(card, "key").value));
        // A new field must not accidentally pick up submissions belonging to a previously deleted key.
        let key;
        do { key = `field_${crypto.getRandomValues(new Uint32Array(2)).join("_")}`; } while (used.has(key));
        const card = appendField({ key, label: "", type: "string" });
        refresh();
        control(card, "label").focus();
        status.textContent = "項目を追加しました。保存すると反映されます。";
    });

    fields.addEventListener("input", () => {
        showError("");
        refresh();
        validate();
    });
    fields.addEventListener("change", (event) => {
        const card = event.target.closest(".schema-field");
        if (card) updateType(card);
        validate();
    });
    fields.addEventListener("click", (event) => {
        const button = event.target.closest("button");
        if (!button) return;
        const card = button.closest(".schema-field");
        if (!card) return;
        const name = control(card, "label").value || "この項目";
        if (button.matches("[data-remove-field]")) {
            if (!window.confirm(`「${name}」を削除しますか？設定を保存するまで変更は反映されません。`)) return;
            const neighbor = card.nextElementSibling || card.previousElementSibling;
            card.remove();
            refresh();
            validate();
            (neighbor ? control(neighbor, "label") : addButton).focus();
            status.textContent = `「${name}」を削除しました。`;
            return;
        }
        if (button.matches("[data-move-up]") && card.previousElementSibling) {
            fields.insertBefore(card, card.previousElementSibling);
        } else if (button.matches("[data-move-down]") && card.nextElementSibling) {
            fields.insertBefore(card.nextElementSibling, card);
        } else return;
        refresh();
        if (button.disabled) control(card, "label").focus();
        else button.focus();
        status.textContent = `「${name}」を${cards().indexOf(card) + 1}番目に移動しました。`;
    });

    form.addEventListener("submit", (event) => {
        validate();
        if (!form.reportValidity()) {
            event.preventDefault();
            return;
        }
        const schema = { fields: cards().map(card => {
            const value = name => control(card, name).value;
            const field = { key: value("key"), label: value("label"), description: value("description") };
            if (value("type") === "file") {
                field.type = "file";
                if (value("accept")) field.accept = value("accept");
            } else {
                field.type = "string";
                field.multiline = value("type") === "multiline";
                if (value("max_length")) field.max_length = Number(value("max_length"));
            }
            return field;
        }) };
        const json = JSON.stringify(schema);
        if (encoder.encode(json).length > 65536) {
            event.preventDefault();
            showError("項目設定全体が64 KiBを超えています。項目数や説明の長さを減らしてください。");
            error.scrollIntoView({ block: "center" });
            return;
        }
        definition.value = json;
    });

    // Run validation in our submit handler so errors are refreshed before the browser checks them.
    form.noValidate = true;
    source.hidden = true;
    editor.hidden = false;
    addButton.hidden = false;
    refresh();
})();

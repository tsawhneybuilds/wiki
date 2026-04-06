ObjC.import("Foundation");

function env(name) {
  const raw = $.getenv(name);
  return raw ? ObjC.unwrap(raw) : "";
}

function writeText(path, text) {
  const nsText = $(text);
  nsText.writeToFileAtomicallyEncodingError($(path), true, $.NSUTF8StringEncoding, null);
}

function toIso(value) {
  try {
    if (!value) {
      return null;
    }
    return new Date(String(value)).toISOString();
  } catch (_) {
    return null;
  }
}

function extractTags(text) {
  const matches = String(text || "").match(/(^|\s)#([\p{L}\p{N}_-]+)/gu) || [];
  return [...new Set(matches.map((entry) => entry.trim().slice(1)))];
}

function notePayload(accountName, folderPath, note) {
  let bodyHtml = "";
  let plaintext = "";
  let creationDate = null;
  let modificationDate = null;
  let passwordProtected = false;
  let shared = false;

  try {
    bodyHtml = String(note.body());
  } catch (_) {}

  try {
    plaintext = String(note.plaintext());
  } catch (_) {}

  try {
    creationDate = toIso(note.creationDate());
  } catch (_) {}

  try {
    modificationDate = toIso(note.modificationDate());
  } catch (_) {}

  try {
    passwordProtected = Boolean(note.passwordProtected());
  } catch (_) {}

  try {
    shared = Boolean(note.shared());
  } catch (_) {}

  return {
    id: String(note.id()),
    name: String(note.name()),
    account: accountName,
    folder_path: folderPath,
    creation_date: creationDate,
    modification_date: modificationDate,
    body_html: bodyHtml,
    plaintext,
    password_protected: passwordProtected,
    shared,
    tags: extractTags(plaintext),
  };
}

function walkFolders(accountName, folders, parentPath, sink) {
  folders.forEach((folder) => {
    const nextPath = parentPath.concat([String(folder.name())]);
    let notes = [];
    try {
      notes = folder.notes();
    } catch (_) {}

    notes.forEach((note) => {
      sink.push(notePayload(accountName, nextPath, note));
    });

    try {
      if (folder.folders) {
        walkFolders(accountName, folder.folders(), nextPath, sink);
      }
    } catch (_) {}
  });
}

function run(argv) {
  const outputPath = argv[0];
  const accountFilter = env("APPLE_NOTES_ACCOUNT_FILTER");
  const Notes = Application("Notes");
  Notes.includeStandardAdditions = true;

  const notes = [];
  Notes.accounts().forEach((account) => {
    const name = String(account.name());
    if (accountFilter && name !== accountFilter) {
      return;
    }
    walkFolders(name, account.folders(), [], notes);
  });

  writeText(
    outputPath,
    JSON.stringify(
      {
        exported_at: new Date().toISOString(),
        notes,
      },
      null,
      2,
    ),
  );

  return outputPath;
}

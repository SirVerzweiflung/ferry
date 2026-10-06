# Windows SmartScreen and Defender warnings

Two different Windows features can complain about Ferry. They have different causes and fixes.

## 1. "Windows protected your PC" (SmartScreen) when starting the installer

This is **reputation-based**. Windows warns about any program that is not code-signed by a
publisher with an established reputation, no matter what the program does. It only appears for
files that carry the "downloaded from the internet" mark (Mark of the Web).

* **For your own PCs:** click *More info → Run anyway*. Or remove the download mark first:
  right-click the file → *Properties* → tick **Unblock**, or in PowerShell run
  `Unblock-File .\FerrySetup-0.2.1.exe`. Files copied over a network share or USB stick usually
  don't carry the mark at all.
* **To make it go away for everyone:** sign the installer (see section 3).

## 2. Defender reports a "threat" after Ferry starts

This is a **heuristic (machine-learning) false positive**. It typically shows a name ending in
`!ml`, for example `Trojan:Win32/Wacatac…!ml`. Ferry does what a clipboard sync tool has to do:
it watches the clipboard, listens on the network and starts with Windows. A clipboard stealer
does the same things, so heuristics look for further hints.

**v0.2 removes the hints that Ferry itself gave:**

| Before (v0.1) | Now |
|---|---|
| Windows functions resolved at run time (`LoadLibrary`/`GetProcAddress`), so the import table was nearly empty. Malware hides its API use this way. | Normal imports: every function Ferry uses is visible in the `.exe`. |
| No version information in the `.exe`. | Version info (product, description, version) and a standard application manifest. |
| The installer started `powershell -ExecutionPolicy Bypass` to edit PATH. | No scripts: PATH is updated with plain registry calls. |
| Only the installer could be signed. | `package-windows.sh` signs `ferry.exe`, `ferryd.exe` **and** the installer. |

This should reduce false positives considerably, but unsigned new software can still be flagged.
Every build has a new hash with no reputation yet. If it happens again:

* *Windows Security → Virus & threat protection → Protection history* → the entry → **Actions →
  Allow on device**.
* **Report the false positive** to Microsoft: <https://www.microsoft.com/en-us/wdsi/filesubmission>
  (choose "Software developer" and "Incorrectly detected"). Microsoft usually clears the file
  within a few days, and repeated clean submissions build reputation.
* Avoid permanently excluding folders from Defender; allowing the specific detection is enough.

## 3. Code signing (the real fix)

`package-windows.sh` signs automatically when a certificate is configured:

```bash
FERRY_SIGN_PFX=~/keys/codesign.pfx FERRY_SIGN_PASS=... ./package-windows.sh
```

Since 2023, publicly trusted code-signing keys must live on a hardware token or in a cloud HSM, so
a plain `.pfx` file only works for certificates you created yourself. Options:

| Option | Cost | Notes |
|---|---|---|
| **SignPath Foundation** | free for open source | The project must be public and built automatically in CI (e.g. the included GitHub workflow); SignPath signs the CI output. Best fit if you publish Ferry. |
| **Certum Open Source Code Signing** | low yearly fee | For individual open-source developers in the EU; cloud signing (SimplySign). Tools like `jsign` can use it from Linux. |
| **Azure Artifact Signing** (formerly Trusted Signing) | ~$10/month | Currently for organisations in the EU/UK and individuals in the USA/Canada only. |
| Regular OV certificate from a CA | ~€100–400/year | Works everywhere; also needs a token or cloud key. |

Signing gives the program a verified publisher right away. The SmartScreen prompt can still appear
for a while until that publisher has built up reputation (downloads without complaints).

A self-signed certificate only helps on PCs where you install it as trusted yourself. That makes
your own key a root of trust for that PC, so keep it safe or don't do it.

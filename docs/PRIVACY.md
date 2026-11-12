# Privacy

Everything Excavate reads stays on your PC. Nothing is uploaded, and every model runs locally. Excavate goes online
only to download the models you choose, Everything if you ask it to install it, and, with **Update automatically** on
(Settings → About), to check once a day for a new version, which is used only if its signature checks out.

## What counts as sensitive

As it reads a file or an image, Excavate looks for:

- **Card numbers** (checked with the Luhn formula), **IBANs** (checked with their mod-97 digits), **ID and passport
  numbers**, including the machine-readable lines on passports;
- **Credentials**: API keys and tokens by their known beginnings (`AKIA…`, `ghp_…`, `xox…`, `sk-…`, JSON web tokens),
  private-key blocks, and values next to or under labels such as Password, PIN, CVV or Recovery code, in a dozen
  languages;
- **Wi-Fi QR codes**: in an image whose text is about a Wi-Fi network, a QR code is treated as its password;
- **Recovery phrases**: 12 or 24 words in a row from the BIP-39 list.

## What happens to them

- **In the index**, the secret is replaced by a label before anything is stored. "Wi-Fi password" still finds the
  screenshot, because the word "password" is there; the password itself isn't.
- **In previews**, the secret itself is pixelated in an image (not its whole line), a Wi-Fi QR code as a whole, and
  secrets in text are masked. **Reveal** asks Windows Hello first, and what was revealed is hidden again the next time
  the popup opens.
- **While you present** (a slide show, or Windows' presentation mode), previews show no images or text. Settings →
  Privacy turns this off.

## What it can't promise

Detection isn't perfect: a password written without a label, in a language Excavate doesn't know, or misread in a
blurry photo can get through. What it reads still stays on your PC either way.

The index is in `%LOCALAPPDATA%\Excavate`. Deleting that folder deletes everything Excavate has read.

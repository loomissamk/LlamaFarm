# Οδηγός Ενημέρωσης και Απεγκατάστασης στο macOS

Αυτή η σελίδα τεκμηριώνει τις υποστηριζόμενες διαδικασίες ενημέρωσης και απεγκατάστασης του LlamaFarm στο macOS (OS X).

Τελευταία επαλήθευση: **22 Φεβρουαρίου 2026**.

## 1) Έλεγχος τρέχουσας μεθόδου εγκατάστασης

```bash
which llamafarm
llamafarm --version
```

Τυπικές τοποθεσίες:

- Homebrew: `/opt/homebrew/bin/llamafarm` (Apple Silicon) ή `/usr/local/bin/llamafarm` (Intel)
- Cargo/bootstrap/χειροκίνητη: `~/.cargo/bin/llamafarm`

Αν υπάρχουν και οι δύο, η σειρά `PATH` του shell σας καθορίζει ποια εκτελείται.

## 2) Ενημέρωση στο macOS

### Α) Εγκατάσταση μέσω Homebrew

```bash
brew update
brew upgrade llamafarm
llamafarm --version
```

### Β) Εγκατάσταση μέσω Clone + bootstrap

Από τον τοπικό κλώνο του αποθετηρίου:

```bash
git pull --ff-only
./bootstrap.sh --prefer-prebuilt
llamafarm --version
```

Αν θέλετε ενημέρωση μόνο από πηγαίο κώδικα:

```bash
git pull --ff-only
cargo install --path . --force --locked
llamafarm --version
```

### Γ) Χειροκίνητη εγκατάσταση προκατασκευασμένου binary

Επαναλάβετε τη ροή λήψης/εγκατάστασης με το πιο πρόσφατο αρχείο έκδοσης και επαληθεύστε:

```bash
llamafarm --version
```

## 3) Απεγκατάσταση στο macOS

### Α) Διακοπή και αφαίρεση υπηρεσίας background πρώτα

Αυτό αποτρέπει τη συνέχεια εκτέλεσης του daemon μετά την αφαίρεση του binary.

```bash
llamafarm service stop || true
llamafarm service uninstall || true
```

Αντικείμενα υπηρεσίας που αφαιρούνται από την `service uninstall`:

- `~/Library/LaunchAgents/com.llamafarm.daemon.plist`

### Β) Αφαίρεση binary ανά μέθοδο εγκατάστασης

Homebrew:

```bash
brew uninstall llamafarm
```

Cargo/bootstrap/χειροκίνητη (`~/.cargo/bin/llamafarm`):

```bash
cargo uninstall llamafarm || true
rm -f ~/.cargo/bin/llamafarm
```

### Γ) Προαιρετικά: αφαίρεση τοπικών δεδομένων εκτέλεσης

Εκτελέστε αυτό μόνο αν θέλετε πλήρη εκκαθάριση ρυθμίσεων, προφίλ auth, logs και κατάστασης workspace.

```bash
rm -rf ~/.llamafarm
```

## 4) Επαλήθευση ολοκλήρωσης απεγκατάστασης

```bash
command -v llamafarm || echo "llamafarm binary not found"
pgrep -fl llamafarm || echo "No running llamafarm process"
```

Αν το `pgrep` εξακολουθεί να βρίσκει διεργασία, σταματήστε την χειροκίνητα και ελέγξτε ξανά:

```bash
pkill -f llamafarm
```

## Σχετικά Έγγραφα

- [One-Click Bootstrap](../one-click-bootstrap.md)
- [Αναφορά Εντολών](../commands-reference.md)
- [Αντιμετώπιση Προβλημάτων](../troubleshooting.md)

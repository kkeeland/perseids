#!/bin/sh
# Perseids installer:  curl -fsSL https://raw.githubusercontent.com/kkeeland/perseids/main/install.sh | sh
# Clones Perseids into ~/.perseids (or updates it) and links the command into ~/.local/bin. No sudo.
# PERSEIDS_HOME changes the folder, PERSEIDS_REPO the clone URL.
set -eu

main() { # all in a function, so a half-downloaded script runs nothing
  repo=${PERSEIDS_REPO:-https://github.com/kkeeland/perseids}
  dir=${PERSEIDS_HOME:-$HOME/.perseids}
  bin=$HOME/.local/bin

  missing=
  for c in git curl ssh jq ruby; do command -v "$c" >/dev/null 2>&1 || missing="$missing $c"; done
  if command -v ruby >/dev/null 2>&1 && ! ruby -e 'exit Gem::Version.new(RUBY_VERSION) >= Gem::Version.new("2.6")'; then
    echo "perseids: needs Ruby 2.6 or later, found $(ruby -e 'print RUBY_VERSION')"
    missing="$missing ruby"
  fi
  if [ -n "$missing" ]; then
    echo "perseids: missing:$missing. Install with:"
    if [ "$(uname -s)" = Darwin ]; then echo "  brew install$missing"
    elif command -v apt-get >/dev/null 2>&1; then echo "  sudo apt-get install -y$(echo "$missing" | sed 's/ssh/openssh-client/')"
    else echo "  your package manager, then run this again"
    fi
    exit 1
  fi

  if [ -d "$dir/.git" ]; then git -C "$dir" pull --ff-only --quiet
  else git clone --quiet "$repo" "$dir"
  fi
  mkdir -p "$bin"
  ln -sf "$dir/bin/perseids" "$bin/perseids"
  echo "perseids: installed $(git -C "$dir" rev-parse --short HEAD) in $dir, linked from $bin/perseids"

  case ":$PATH:" in
    *":$bin:"*) ;;
    *)
      case ${SHELL:-} in */zsh) rc='~/.zshrc' ;; */bash) rc='~/.bashrc' ;; *) rc='~/.profile' ;; esac
      echo "perseids: $bin isn't on your PATH. Add it with:"
      echo "  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> $rc && . $rc"
      ;;
  esac
  echo "perseids: next, run  perseids key  to save your Boat API key (boat.dev, Settings, API keys)."
}

main

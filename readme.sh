set -e

function sha {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum
    else
        shasum -a 256
    fi
}

if [ "$1" = "check" ]; then
    echo "Checking if README.md is up to date with src/lib.rs"
    lib_hash=$(cargo readme --no-indent-headings | sha)
    readme_hash=$(cat README.md | sha)
    if [ "$lib_hash" = "$readme_hash" ]; then
        echo "README.md is up to date"
    else
        echo "README.md is out of date."
        echo "Please run $0 script."
        exit 1
    fi
else
    echo "Generating README.md"
    cargo readme --no-indent-headings > README.md
fi

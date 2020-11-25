set -e

if [ "$1" = "check" ]; then
    echo "Checking if README.md is up to date with src/lib.rs"
    lib_hash=$(cargo readme --no-indent-headings | sha256sum)
    readme_hash=$(cat README.md | sha256sum)
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

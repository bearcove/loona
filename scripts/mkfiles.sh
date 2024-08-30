#!/usr/bin/env -S bash -euo pipefail

# Change to the script's directory
cd "$(dirname "$0")"

# Define an array of file names and sizes
declare -A files=(
    ["4M"]="4M"
    ["16M"]="16M"
    ["256M"]="256M"
    ["1GB"]="1G"
)

# Print summary and ask for consent
echo -e "\e[1;33mThis script will generate the following files in /tmp/stream-file:\e[0m"
for file in "${!files[@]}"; do
    echo "- $file (${files[$file]})"
done
echo -e "\n\e[1;33mDo you want to proceed? (y/n)\e[0m"
read -r consent

if [[ ! $consent =~ ^[Yy]$ ]]; then
    echo "Aborting script execution."
    exit 1
fi

# Create directory if it doesn't exist
mkdir -p /tmp/stream-file

# Loop through the array and generate files
for file in "${!files[@]}"; do
    echo -e "\e[1;34mGenerating $file file...\e[0m"
    dd if=/dev/urandom of="/tmp/stream-file/$file" bs=${files[$file]} count=1 status=progress
    echo -e "\e[1;32mCompleted generating $file file!\e[0m"
done

echo -e "\e[1;35mAll files have been generated successfully in /tmp/stream-file\e[0m"

sudo apt update && sudo apt install -y ca-certificates curl

sudo install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://apt.fsend.sh/key.asc \
  | sudo tee /etc/apt/keyrings/fsend.asc >/dev/null

echo "deb [signed-by=/etc/apt/keyrings/fsend.asc] https://apt.fsend.sh stable main" \
  | sudo tee /etc/apt/sources.list.d/fsend.list

sudo apt update && sudo apt install fsend

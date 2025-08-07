
env:
PROJECT_NAME = 'test_project' // use for folder name can be change for few project in one service
API_ADDRESS = 0.0.0.0:3000 // Use for set Api address server


🛠 Configuration for GIT SSH


Step 1: Generate an SSH key (if you don't have one)
```bash
ssh-keygen -t ed25519 -C "bot@mr-ai.com" -f ./ssh_keys/bot_key
```

The following will be created:

Private key: `~/.ssh/id_ed25519`

Public key: `~/.ssh/id_ed25519.pub`

Step 2: Add the public key to GitLab/GitHub
GitHub: Settings → SSH and GPG keys → New SSH key

GitLab: User Settings → SSH Keys → Add
`ssh-keyscan gitlab.com > ~/.ssh/known_hosts`

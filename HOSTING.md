# Hosting Kiku's models

`kiku-engine` is Kiku's model host. Run it on a Linux PC or server, give Kiku on Windows its code, and the
speech model runs there instead of on your PC. Nothing is sent anywhere else, and the connection is
encrypted and tied to the host's own certificate.

## From the tarball

It is built on Ubuntu 22.04, so it needs glibc 2.35 or newer (Ubuntu 22.04, Debian 12 or newer).
Unpack it and go into the folder. Models go in `~/.local/share/kiku/models` (set `KIKU_MODELS_DIR` to put
them elsewhere).

    ./kiku-engine --models                                    # what's on offer and what's installed
    ./kiku-engine --download parakeet-unified --device gpu    # or --device cpu
    ./kiku-engine --serve

`--serve` prints one line, `kiku-host kiku://<address>:7300/#<fingerprint>.<secret>`. That is the host's
code: paste it into Kiku's settings on Windows (Listening, a model on another computer). The host keeps
its identity in `~/.local/share/kiku/host` (`--state <folder>` changes it), so the code stays the same
after a restart. `--listen <address:port>` changes where it listens (default `0.0.0.0:7300`).

The GPU needs NVIDIA's driver with the CUDA 12.9 libraries and cuDNN 9 installed (older CUDA 12 releases
load but quietly run on the CPU). Without them, or without a card, use `--device cpu`.

## With Docker

The image has CUDA 12.9 and cuDNN 9, so a GPU host needs only NVIDIA's driver and its container toolkit.
Models and the host's identity live in the volume `kiku`.

    docker run --rm --init -v kiku:/data ghcr.io/tannhausercommons/kiku-engine --download parakeet-unified --device gpu
    docker run -d --init --name kiku --restart unless-stopped --gpus all -p 7300:7300 -v kiku:/data ghcr.io/tannhausercommons/kiku-engine
    docker logs kiku                                          # the code is in the first lines

Leave out `--gpus all` and use `--device cpu` for a host without a GPU.

## As a service

Make a user for it and put the unpacked folder in `/opt/kiku-engine`. Models and the host's identity follow that
user's home, so download the models as it:

    sudo useradd --system --create-home kiku
    sudo -u kiku -H /opt/kiku-engine/kiku-engine --download parakeet-unified --device gpu

Save this as `/etc/systemd/system/kiku-engine.service`, with the folder changed if you put it elsewhere:

    [Unit]
    Description=Kiku model host
    After=network-online.target
    Wants=network-online.target

    [Service]
    User=kiku
    ExecStart=/opt/kiku-engine/kiku-engine --serve
    Restart=on-failure
    RestartSec=5

    [Install]
    WantedBy=multi-user.target

Then `sudo systemctl enable --now kiku-engine`, and read the code with `journalctl -u kiku-engine`.

## Reaching it from elsewhere

The code holds the address the host listens on, which is usually not the one your PC can reach. Edit the
address in the code before pasting it into Kiku: change only the part between `kiku://` and `/#`.

- On a LAN, use the host's name or address, like `kiku://studio:7300/#...`.
- Over the internet, use Tailscale: install it on both computers and use the host's Tailscale name or
  address. Or forward port 7300 on your router to the host, though a forwarded port is open to everyone
  to try.

## Keep the code private

The code's secret is what lets someone use your host, so treat it like a password: don't post it, and
share it only with people who may use your GPU. To change it, stop the host, delete the `secret` file in
its state folder (the Docker volume's `host` folder), and start it again. Old codes then stop working.

## Checking a host

From any computer with `kiku-engine`, `--check` connects to a host, has it load a model and listen to a
short test clip, and says how long that took and what it heard:

    read -rs KIKU_HOST_CODE; export KIKU_HOST_CODE            # paste the code; it isn't shown or kept in history
    ./kiku-engine --check --device gpu

Without a code on the command line, `--check` reads `KIKU_HOST_CODE`. The code can also follow `--check`
(`./kiku-engine --check "kiku://..."`), but that form lands in your shell history and `ps`.

`--model <id>` picks another model (default `parakeet-unified`) and `--device cpu|gpu` the device (default
`cpu`). It exits with an error saying why when the host can't be reached or can't load the model.

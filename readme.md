This is a framework for self-updating read-only [NixOS](https://nixos.org/) systems.

**Features**

- Self updating
- Read-only (changes to the system are discarded at reboot)
- Uses grub fallback if a version can't boot properly

There are two components, a version image and an installer.

The installer is used for initial system setup and contains the first system version you define.

Subsequent versions you upload to a file server (right now, any S3 compatible).

When the system boots, it checks for a newer version from the file server and updates.

# Usage

`default.nix` can be used directly from `nix-build` or your flake or however you'd like.

It defines a function that takes a system config (path) plus a number of other parameters describing how to create a version, and produces a number of attributes with derivations for the images and scripts you'll need.

## Defining a system configuration

### Your system must include

- A hostname
- Automatic network setup -- organixm needs network access on boot to update
- Services, system packages

### Your system must _not_ include

- Filesystems on your main disk. The main disk is partitioned and managed by organixm. If you have additional non-boot disks you can define filesystems for those, although you'll need to provide services to make sure they're formatted first.

### Additional available configuration

- `config.extraRootFiles`: a list of attribute sets with the following fields
  - `source`: A derivation
  - `target`: A string path indicating a location on the boot disk root filesystem to copy the source to
  - `mode`: A string like `0600` indicating installed file permissions
  - `user`: A user name for the owner of the installed files
  - `group`: A group name for the owner of the installed files

I mostly use this for enabling linger on systemd user profiles, since there's no other mechanism for this at the moment.

## Base usage

You have your system configuration, let's call it `kiosk.nix`.

The base command for building everything is

```
nix-build https://github.com/andrewbaxter/organixm/archive/master.tar.gz \
  --arg version_config ./kiosk.nix \
  --argstr version_uuid ABCD-EFGH-IJKL-MNOPQR-SOMETHING \
  --argstr region us-east-1 \
  --argstr bucket myorganixms \
  --argstr object_path kiosk/pos \
  --argstr ro_access_key **** \
  --argstr ro_secret_key **** \
  --argstr successUnit my-kiosk.service \
  --arg max_size 10
```

You need a few more params for it to be useful though.

See `default.nix` for parameter descriptions, or in case this gets out of sync with the code (really wish Github let you import clips of source code).

(if you have a flake or you'd like to wrap it in your own expression, you can use `pkgs.fetchFromGithub` or similar to pin a specific version)

## Preparing the installer with the initial version

Call the above with the additional parameters

- `-A config.system.build.installer -o installer`

  This will produce a symlink to the installer ISO image. `sudo cp installer /dev/disk/by-partlabel/my-usb-drive`

To use the image, just boot into it. It will automatically format the first disk it sees and install the version bundled with it, then shut down the computer.

Once the computer shuts down, remove the installer device and start it again and it should boot into your initial version.

### Testing in Qemu

You can test the installer locally by running:

```
qemu-img create -f qcow2 root.qcow2 50G
qemu-system-x86_64 -machine q35 -nic user,model=virtio-net-pci -m 1024 \
	-drive if=virtio,file=root.qcow2,id=myhd,format=qcow2 \
	-cdrom installer/iso/nixos.iso -boot d \
	-display sdl -serial mon:stdio
```

Once that runs successfully, you can test the installed host with

```
qemu-system-x86_64 -machine q35 -nic user,model=virtio-net-pci -m 1024 \
    -drive if=virtio,file=root.qcow2,id=myhd,format=qcow2 \
    -display sdl -serial mon:stdio
```

### AWS-like template images

Note, if you want to use this on services like AWS you'll need to make a template system image. In the future it would be nice to produce such images here, but you can try templating an image yourself by doing the above Qemu test and uploading the `root.qcow2`.

## Preparing a new version

Call the base command with the additional parameters

- `-A config.system.build.upload -o upload`
  Will produce a script `upload` which you can call to upload the image. You need to set up the access/secret key environment variables for your file host.

OR

- `-A config.system.build.version -o version` and `-A config.system.build.version_meta -o version_meta`
  Alternatively, this symlinks the generated version image to `version` which you can upload yourself. You also need to upload `version_meta` to the object path specified on arguments with a suffix of `.meta` (ex: `kiosk/pos.meta`)

# Architecture

## The update mechanism

The bare metal provisioner

- creates two OS partitions
- downloads the latest image and writes it to both partitions
- points grub to one of the partitions

The cloud provisioner is simply a template of an already-provisioned host.

The version image has a service that

- Starts at boot
- Checks for a newer version
- Downloads the image and overwrites the inactive partition
- Updates grub to point to that partition

The read-onlyness is done by

- Mounting `/` read-only at boot
- Mounting an overlay over key read-write directories (`/etc`, mostly for `resolv.conf`)

## Changes from upstream

- `make-disk-image.nix` - the only significant change here is I added a `uuid` parameter to set the root filesystem uuid. I'm not 100% sure this was necessary, it might have been fine with partlabels.

## The future

Some things that didn't quite pan out

- Squashfs: I managed to get the system to boot with some hacks but there were weird issues with nscd, networking, logging in, and various other things breaking. I needed to hack on the NixOS `stage-1-init.sh` among other things. Squashfs would have been cool since they could be put on the same partition to save some space, plus they're smaller and (theoretically) can make boot faster on systems with slow disk io.

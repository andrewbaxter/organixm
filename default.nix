{
  # Path/attrset (passed to imports=), System config to use to build this version
  version_config

, # String, UUID for this version. You can generate it with `uuidgen` and every version needs
  # a unique UUID or else nothing will update (no other known adverse effects from reusing).
  # It's used for the fs UUID, in metadata files, etc.
  version_uuid

, # String, S3 region (us-east-1) or S3-compatible endpoint (sg1.your.host) to pull new versions
  version_region

, # String, Bucket from which to pull new versions
  version_bucket

, # String, Object in bucket from which to pull new versions; meta must be located at same path plus .meta
  version_object_path

, # String, Access key for pulling new versions (read only - added to version image for self update)
  version_ro_access_key

, # String, Secret key for pulling new versions (read only - added to version image for self update)
  version_ro_secret_key

, # String, Systemd unit whose success indicates a successful boot (prevent grub fallback)
  version_success_unit

, # Int (GiB), The expected max size of all versions from here out (used to decide size of root partition).
  # Only used by installer image.
  version_max_size

}:
let
  build_system = (configuration:
    let
      eval = import <nixpkgs/nixos/lib/eval-config.nix> {
        system = builtins.currentSystem;
        modules = [ configuration ];
      };
    in
    {
      inherit (eval) pkgs config options;
      system = eval.config.system.build.toplevel;
      inherit (eval.config.system.build) vm vmWithBootLoader;
    });
  version = (build_system
    ({ config, modulesPath, pkgs, lib, ... }:
      let
        joinlines = (list: lib.lists.fold (a: b: a + "\n" + b) "" list);
      in
      {
        imports = [
          <nixpkgs/nixos/modules/profiles/all-hardware.nix>
          version_config
        ];

        options = {
          extraRootFiles = lib.mkOption {
            type = lib.types.listOf (lib.types.submodule {
              options = {
                source = lib.mkOption {
                  type = lib.types.str;
                  description = "Tree to place";
                };
                target = lib.mkOption {
                  type = lib.types.str;
                  description = "Where to place it";
                };
                mode = lib.mkOption {
                  type = lib.types.str;
                  description = "Tree mode";
                };
                user = lib.mkOption {
                  type = lib.types.str;
                  description = "Tree user";
                };
                group = lib.mkOption {
                  type = lib.types.str;
                  description = "Tree group";
                };
              };
            });
            default = [ ];
            description = "Extra files to place on root";
          };
        };

        config = {
          boot = {
            loader = {
              grub = {
                enable = false;
              };
            };
            initrd = {
              kernelModules = [ "overlay" ];
            };
            kernelModules = [ "overlay" ];
          };
          networking = {
            useDHCP = true;
          };

          fileSystems = {
            "/" = {
              device = "UUID=${version_uuid}";
              fsType = "ext4";
              options = [ "ro" ];
            };
            "/tmp" = {
              fsType = "tmpfs";
              options = [ "rw" "size=100m" "inode64" "uid=0" "gid=0" "mode=777" "nosuid" "nodev" ];
            };
            "/tmp-overlays" = {
              fsType = "tmpfs";
              options = [ "rw" "size=1m" "uid=0" "gid=0" "mode=755" "nosuid" ];
              neededForBoot = true;
            };
            "/etc" = {
              device = "overlay";
              fsType = "overlay";
              options = [ "lowerdir=/etc" "upperdir=/tmp-overlays/etc/upper" "workdir=/tmp-overlays/etc/work" ];
              depends = [ "/tmp-overlays" "/" ];
              neededForBoot = true;
            };
            "/rw" = {
              fsType = "ext4";
              # PARTLABEL= doesn't work (not found) for some reason
              device = "/dev/disk/by-partlabel/rw";
              options = [ "rw" "noatime" ];
              depends = [ "/" ];
              neededForBoot = true;
            };
            "/var" = {
              device = "overlay";
              fsType = "overlay";
              options = [ "lowerdir=/var" "upperdir=/rw/overlays/var/upper" "workdir=/rw/overlays/var/work" ];
              depends = [ "/rw" "/" ];
              neededForBoot = true;
            };
            "/root" = {
              device = "overlay";
              fsType = "overlay";
              options = [ "lowerdir=/root" "upperdir=/rw/overlays/root/upper" "workdir=/rw/overlays/root/work" ];
              depends = [ "/rw" "/" ];
            };
            "/home" = {
              device = "overlay";
              fsType = "overlay";
              options = [ "lowerdir=/home" "upperdir=/rw/overlays/home/upper" "workdir=/rw/overlays/home/work" ];
              depends = [ "/rw" "/" ];
            };
          };

          systemd = {
            services = {
              premount-rw-overlays =
                let
                  undeps = [ "var.mount" "root.mount" "home.mount" ];
                in
                {
                  after = [ "rw.mount" ];
                  before = undeps;
                  wantedBy = undeps;
                  unitConfig = {
                    DefaultDependencies = "no";
                  };
                  serviceConfig = {
                    Type = "oneshot";
                    ExecStart = pkgs.writeShellScript "premount-rw-overlays-script" ''
                      mkdir -p /rw/overlays/home/work
                      mkdir -p /rw/overlays/home/upper
                      mkdir -p /rw/overlays/var/work
                      mkdir -p /rw/overlays/var/upper
                      mkdir -p /rw/overlays/root/work
                      mkdir -p /rw/overlays/root/upper
                    '';
                  };
                };
              organixm_update = {
                wantedBy = [ "multi-user.target" ];
                description = "organixm-update";
                path = [
                  pkgs.grub2
                  pkgs.util-linux
                  pkgs.iproute2
                ];
                serviceConfig = {
                  Type = "oneshot";
                  ExecStart = "${config.system.build.tools}/bin/update";
                };
              };
              organixm_success = {
                wantedBy = [ "multi-user.target" ];
                description = "organixm-success";
                after = [ version_success_unit "organixm_update.service" ];
                requires = [ version_success_unit ];
                path = [
                  pkgs.grub2
                  pkgs.util-linux
                ];
                serviceConfig = {
                  Type = "oneshot";
                  ExecStart = "${config.system.build.tools}/bin/success";
                };
              };
            };
          };

          system = {
            build =
              let
                internal_meta = {
                  region = version_region;
                  bucket = version_bucket;
                  object_path = version_object_path;
                  access_key = version_ro_access_key;
                  secret_key = version_ro_secret_key;
                  uuid = version_uuid;
                  der_bzimage = "${config.system.build.kernel}/bzImage";
                  der_init = "${config.system.build.toplevel}/init";
                  der_initrd = "${config.system.build.initialRamdisk}/initrd";
                };
              in
              rec {
                tools = pkgs.callPackage (import ./tools.nix) { };
                image = import ./patched/nixos/lib/make-disk-image.nix {
                  rootUuid = version_uuid;
                  format = "raw";
                  installBootLoader = false;
                  partitionTableType = "none";
                  contents =
                    let
                      empty_dir = pkgs.runCommand "empty-dir" { } ''mkdir $out'';
                    in
                    [
                      {
                        source = pkgs.writeTextFile {
                          name = "internal-meta";
                          text = builtins.toJSON internal_meta;
                        };
                        target = "/organixm.json";
                        mode = "0600";
                        user = "root";
                        group = "root";
                      }
                      {
                        source = empty_dir;
                        target = "/boot";
                        mode = "0600";
                        user = "root";
                        group = "root";
                      }
                      {
                        source = empty_dir;
                        target = "/rw";
                        mode = "0600";
                        user = "root";
                        group = "root";
                      }
                      {
                        source = empty_dir;
                        target = "/tmp-overlays";
                        mode = "0600";
                        user = "root";
                        group = "root";
                      }
                    ] ++ config.extraRootFiles;
                  postVM = ''
                    trim() {
                      tr -d '\n' | sed -e 's/^[[:space:]]\+\(.*[^[:space:]]\)[[:space:]]\+$/\1/'
                    }
                    stat -c %s $out_filename | trim > $out/size
                    sha256sum $out_filename | sed -e "s/ .*//" | trim > $out/sha256
                    zstd $out_filename -o $out/image.zstd
                    rm $out_filename
                  '';
                  inherit config lib pkgs;
                };
                image_path = "${config.system.build.image}/image.zstd";
                external_meta =
                  let
                    sha256_ctx = builtins.readFile "${config.system.build.image}/sha256";
                    size_ctx = builtins.readFile "${config.system.build.image}/size";
                  in
                  pkgs.writeTextFile {
                    name = "external-meta";
                    # Nice leaky abstractions there, wouldn't want something to happen to them
                    text = lib.lists.fold lib.strings.addContextFrom
                      (builtins.toJSON {
                        sha256 = builtins.unsafeDiscardStringContext sha256_ctx;
                        size = lib.strings.toInt (builtins.unsafeDiscardStringContext size_ctx);
                        format = "raw+zstd";
                        internal = internal_meta;
                      }) [ sha256_ctx size_ctx ];
                  };
              };
          };
        };
      })).config.build.system;
in
build_system
  ({ config, modulesPath, pkgs, lib, ... }:
  {
    config = {
      system = {
        build = {
          # Path to the built image (compressed raw) and meta
          version = version.image_path;
          version_meta = version.external_meta;

          # Script to upload the image to the specified provider.
          # Needs s3-cred env set (access and secret keys)
          upload = pkgs.writeScript
            "upload"
            "${version.tools}/bin/upload ${version.external_meta} ${version.image_path}";

          # Installer image, an iso for usb/cd that will format a system
          # and install the above version
          installer = (build_system ({ config, modulesPath, pkgs, lib, ... }:
            {
              imports = [
                <nixpkgs/nixos/modules/installer/cd-dvd/iso-image.nix>
                <nixpkgs/nixos/modules/profiles/all-hardware.nix>
              ];

              config = {
                isoImage = {
                  makeEfiBootable = true;
                  makeUsbBootable = true;
                };
                boot = {
                  loader = {
                    grub = {
                      device = "/dev/sda";
                    };
                  };
                  kernelParams = [ "console=ttyS0,115200n8" "console=tty0" ];
                  consoleLogLevel = lib.mkDefault 7;
                  initrd = {
                    kernelModules = [ "nvme" ];
                    availableKernelModules = [ "ata_piix" "uhci_hcd" "sr_mod" ];
                  };
                };
                networking = {
                  useDHCP = false;
                  resolvconf = {
                    enable = false;
                  };
                  firewall = {
                    enable = false;
                  };
                  dhcpcd = {
                    enable = false;
                  };
                };
                services = {
                  timesyncd = {
                    enable = false;
                  };
                  journald = {
                    console = "/dev/console";
                  };
                };
                users = {
                  users = {
                    root = {
                      initialPassword = "x";
                    };
                  };
                };
                systemd = {
                  targets = {
                    getty = {
                      enable = false;
                    };
                  };
                  services = {
                    "organixm-assimilate" = {
                      wantedBy = [ "multi-user.target" ];
                      description = "Install system";
                      path = [
                        pkgs.util-linux
                        pkgs.parted
                        pkgs.e2fsprogs
                        pkgs.grub2
                      ];
                      serviceConfig = {
                        Type = "oneshot";
                        ExecStart =
                          let
                            external_meta = builtins.readFile version.external_meta;
                            init_config = pkgs.writeText "install-config" (
                              lib.strings.addContextFrom external_meta (builtins.toJSON {
                                size = version_max_size;
                                version = builtins.fromJSON (builtins.unsafeDiscardStringContext external_meta);
                                version_path = version.image_path;
                              })
                            );
                          in
                          "${version.tools}/bin/init ${init_config}";
                      };
                    };
                    systemd-logind = {
                      enable = false;
                    };
                    systemd-user-sessions = {
                      enable = false;
                    };
                  };
                };
                system = {
                  build = {
                    installer = config.system.build.isoImage;
                  };
                };
              };
            })).config.build.system.installer;
        };
      };
    };
  })


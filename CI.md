# About CI-testing of `ArceOS-UMHV`

The CI-testing of `ArceOS-UMHV` is done using GitHub Actions. The CI configuration file is located at `.github/workflows/test.yml`. The workflow first installs the necessary dependencies, then builds the project, and finally runs the tests.

To run a guest OS in the CI environment:

- A disk image is created using `make disk_img`.
- The image of the guest OS is copied to the disk image. If other files are needed, they are also copied to the disk image.
- A guest config file should be prepared. This file contains the configuration of the guest OS. Some examples of guest config files are located in the `arceos-vmm/configs` directory.
- The hypervisor is then launched with the disk image and the guest config file.

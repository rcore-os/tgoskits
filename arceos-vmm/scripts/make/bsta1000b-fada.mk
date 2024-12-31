A1000B_GITHUB_URL = https://github.com/arceos-hypervisor/platform_tools/releases/download/latest/a1000b.zip
A1000B_MKIMG_FILE = ./tools/a1000b/mkimage
check-download:
ifeq ("$(wildcard $(A1000b_MKIMG_FILE))","")
		@echo "file not found, downloading from $(A1000B_GITHUB_URL)..."; 
		wget $(A1000B_GITHUB_URL); 
		unzip -o a1000b.zip -d tools; 
		rm a1000b.zip; 
endif


fada: check-download build
	gzip -9 -cvf $(OUT_BIN) > arceos-fada.bin.gz
	$(A1000B_MKIMG_FILE) -f ./tools/a1000b/bsta1000b-fada-arceos.its arceos-fada.itb
	@echo 'Built the FIT-uImage arceos-fada.itb'

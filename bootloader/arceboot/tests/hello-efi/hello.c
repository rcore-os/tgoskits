/* Minimal riscv64 UEFI application: prints to ConOut via the system table. */
typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;
typedef u64 EFI_STATUS;
typedef void *EFI_HANDLE;

typedef struct {
    u64 Signature;
    u32 Revision;
    u32 HeaderSize;
    u32 CRC32;
    u32 Reserved;
} EFI_TABLE_HEADER;

typedef struct _EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL {
    EFI_STATUS (*Reset)(struct _EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This,
                        int ExtendedVerification);
    EFI_STATUS (*OutputString)(struct _EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *This,
                               u16 *String);
    void *TestString;
    void *QueryMode;
    void *SetMode;
    void *SetAttribute;
    void *ClearScreen;
    void *SetCursorPosition;
    void *EnableCursor;
    void *Mode;
} EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL;

typedef struct {
    EFI_TABLE_HEADER Hdr;
    u16 *FirmwareVendor;
    u32 FirmwareRevision;
    EFI_HANDLE ConsoleInHandle;
    void *ConIn;
    EFI_HANDLE ConsoleOutHandle;
    EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL *ConOut;
    EFI_HANDLE StandardErrorHandle;
    void *StdErr;
    void *RuntimeServices;
    void *BootServices;
    void *NumberOfTableEntries;
    void *ConfigurationTable;
} EFI_SYSTEM_TABLE;

static u16 msg[] = {
    'H', 'e', 'l', 'l', 'o', ' ', 'f', 'r', 'o', 'm', ' ',
    'a', ' ', 'r', 'i', 's', 'c', 'v', '6', '4', ' ', 'U', 'E', 'F', 'I', ' ',
    'a', 'p', 'p', ' ', 'r', 'u', 'n', 'n', 'i', 'n', 'g', ' ', 'o', 'n', ' ',
    'A', 'r', 'c', 'e', 'B', 'o', 'o', 't', '!', '\r', '\n', 0,
};

EFI_STATUS efi_main(EFI_HANDLE image, EFI_SYSTEM_TABLE *st)
{
    (void)image;
    st->ConOut->OutputString(st->ConOut, msg);
    return 0;
}

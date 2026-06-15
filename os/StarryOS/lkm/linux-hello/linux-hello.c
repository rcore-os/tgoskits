#include <linux/init.h>
#include <linux/module.h>
#include <linux/kernel.h>

MODULE_LICENSE("GPL");
MODULE_AUTHOR("Your Name");
MODULE_DESCRIPTION("A simple Hello World kernel module");
MODULE_VERSION("0.1");


static int my_int = 100;
module_param(my_int, int, 0644);
MODULE_PARM_DESC(my_int, "An integer parameter");

static char *my_string = "default";
module_param(my_string, charp, 0644);
MODULE_PARM_DESC(my_string, "A string parameter");


static int __init hello_init(void)
{
    printk(KERN_INFO "Module parameter my_int: %d\n", my_int);
    printk(KERN_INFO "Module parameter my_string: %s\n", my_string);
    printk(KERN_INFO "Hello, World!\n");
    return 0;
}

static void __exit hello_exit(void)
{
    printk(KERN_INFO "Goodbye, World!\n");
}

module_init(hello_init);
module_exit(hello_exit);
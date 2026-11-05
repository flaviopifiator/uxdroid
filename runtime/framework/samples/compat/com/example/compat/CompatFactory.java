package com.example.compat;

import android.app.AppComponentFactory;
import android.app.Application;

/** The manifest's appComponentFactory, as androidx.core's CoreComponentFactory is for androidx apps. */
public class CompatFactory extends AppComponentFactory {
    @Override
    public Application instantiateApplication(ClassLoader cl, String className)
            throws InstantiationException, IllegalAccessException, ClassNotFoundException {
        System.out.println("factory instantiates " + className + " with the app's class loader "
                + (cl == CompatFactory.class.getClassLoader()));
        return super.instantiateApplication(cl, className);
    }
}

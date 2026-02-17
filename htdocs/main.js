const {Climate} = require('codegen/maison_pb.js');
const {MaisonClient} = require('codegen/maison_grpc_web_pb.js');

class Maison {
    constructor (api) {
        this.api = api;
        this.staleness_expiries = {};
        this.kitchen = [null, null, null];
    }

    run = () => {
        this.run_livetemp();
        this.run_kitchen();
        this.run_boiler();
    }

    display_value = (staleness_key, value, lifetime, setter) => {
        if (this.staleness_expiries.hasOwnProperty(staleness_key)) {
            clearTimeout(this.staleness_expiries[staleness_key]);
            delete this.staleness_expiries[staleness_key];
        }
        setter(value);
        if ((value !== null) && (lifetime !== null)) {
            this.staleness_expiries[staleness_key] = setTimeout(() => {
                this.display_value(staleness_key, null, null, setter);
            }, lifetime);
        }
    }

    subscribe = (make_stream, callback) => {
        var unhealthy = [null];
        this.resubscribe(make_stream, callback, unhealthy);
    }

    resubscribe = (make_stream, callback, unhealthy) => {
        var top = this;
        var restart = [null];
        var stream = make_stream();
        stream.on('data', (response) => {
            if (unhealthy[0] !== null) {
                clearTimeout(unhealthy[0]);
                unhealthy[0] = null;
            }
            callback(response);
        });
        stream.on('status', function(status) {
            if (unhealthy[0] === null) {
                unhealthy[0] = setTimeout(() => { callback(null); }, 5000);
            }
            if (restart[0] === null) {
                restart[0] = setTimeout(() => { top.resubscribe(make_stream, callback, unhealthy); }, 1000);
            }
        });
        stream.on('end', function() {
            if (unhealthy[0] === null) {
                unhealthy[0] = setTimeout(() => { callback(null); }, 5000);
            }
            if (restart[0] === null) {
                restart[0] = setTimeout(() => { top.subscribe(make_stream, callback, unhealthy); }, 0);
            }
        });
    }

    run_livetemp = () => {
        var top = this;
        this.subscribe(() => {
            return this.api.monitorLiveTemperatures(new proto.google.protobuf.Empty(), {});
        }, (response) => {
            var key = (response === null) ? 'temperatures' : ('climate-' + response.getUnit());
            top.display_value(
                key,
                (response === null) ? null : (response.hasTemperature() ? response.getTemperature() : null),
                4500000,
                (v) => {
                    var els = document.getElementsByClassName(key);
                    for (var i = 0; i < els.length; i++) {
                        var livetemp_els = els[i].getElementsByClassName("livetemp");
                        for (var j = 0; j < livetemp_els.length; j++) {
                            livetemp_els[j].textContent = v === null ? '' : (v.toFixed(1) + " \xb0C");
                        }
                    }
                },
            );
        });
    }

    set_kitchen = (response, light_index) => {
        var top = this;
        this.display_value(
                'kitchen_' + light_index,
                (response === null) ? null : (response.hasState() ? response.getState() : null),
                66000000,
                (v) => {
                    top.kitchen[light_index] = v;
                    var known = top.kitchen[0] !== null && top.kitchen[1] !== null && top.kitchen[2] !== null;
                    var on = known && top.kitchen[0] && top.kitchen[1] && top.kitchen[2];
                    var off = known && (!top.kitchen[0]) && (!top.kitchen[1]) && (!top.kitchen[2]);
                    var overall = known ? (on ? "light_on" : (off ? "light_off" : "light_some")) : "light_unknown";
                    var els = document.getElementsByClassName("kitchen_lights");
                    for (var i = 0; i < els.length; i++) {
                        var spans = els[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = overall;
                        }
                    }
                },
        );
    }

    run_kitchen = () => {
        var top = this;
        this.subscribe(() => {
            return this.api.monitorKitchenCeiling(new proto.google.protobuf.Empty(), {});
        }, (response) => {
            top.set_kitchen(response, 0);
        });
        this.subscribe(() => {
            return this.api.monitorKitchenUnderCupboards(new proto.google.protobuf.Empty(), {});
        }, (response) => {
            top.set_kitchen(response, 1);
        });
        this.subscribe(() => {
            return this.api.monitorKitchenUnderStairs(new proto.google.protobuf.Empty(), {});
        }, (response) => {
            top.set_kitchen(response, 2);
        });
    }

    run_boiler = () => {
        var top = this;
        this.subscribe(() => {
            return this.api.monitorBoiler(new proto.google.protobuf.Empty(), {});
        }, (response) => {
            this.display_value(
                'live_heating',
                (response === null) ? null : (response.hasHeating() ? response.getHeating() : null),
                66000000,
                (v) => {
                    var els = document.getElementsByClassName("heating");
                    for (var i = 0; i < els.length; i++) {
                        var spans = els[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = (v === null) ? 'light_unknown' : (v ? 'light_on' : 'light_off');
                        }
                    }
                },
            );
            this.display_value(
                'live_hot_water',
                (response === null) ? null : (response.hasHotWater() ? response.getHotWater() : null),
                66000000,
                (v) => {
                    var els = document.getElementsByClassName("hot_water");
                    for (var i = 0; i < els.length; i++) {
                        var spans = els[i].getElementsByTagName("span");
                        for (var j = 0; j < spans.length; j++) {
                            spans[j].className = (v === null) ? 'light_unknown' : (v ? 'light_on' : 'light_off');
                        }
                    }
                },
            );
        });
    }
}

new Maison(new MaisonClient(URL.parse(window.location).toString())).run();

const {Climate} = require('codegen/maison_pb.js');
const {MaisonClient} = require('codegen/maison_grpc_web_pb.js');

class Maison {
    constructor (api) {
        this.api = api;
        this.staleness_expiries = {};
    }

    run = () => {
        this.run_livetemp();
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

    run_livetemp = () => {
        var top = this;
        var req = new proto.google.protobuf.Empty();
        var stream = this.api.monitorLiveTemperatures(req, {});
        stream.on('data', function(response) {
            var key = 'climate-' + response.getUnit();
            top.display_value(
                key,
                response.hasTemperature() ? response.getTemperature() : null,
                4500000,
                (v) => {
                    var els = document.getElementsByClassName(key);
                    for (var i = 0; i < els.length; i++) {
                        var livetemp_els = els[i].getElementsByClassName("livetemp");
                        for (var j = 0; j < livetemp_els.length; j++) {
                            livetemp_els[j].textContent = v == null ? '' : (v.toFixed(1) + " \xb0C");
                        }
                    }
                }
            );
        });
        stream.on('status', function(status) {
            setTimeout(() => { top.run_livetemp(); }, 1000);
        });
        stream.on('end', function() {
            top.run_livetemp();
        });
    }
}

new Maison(new MaisonClient(URL.parse(window.location).toString())).run();
